#!/usr/bin/env python3
"""Rents an OVH Public Cloud GPU instance for gpui-av runs, with a budget.

The instance runs `agent.py`, which takes shell scripts from a Swift
container and deletes itself at its deadline or when idle (see agent.py).
Every launch and deletion goes in a ledger; a launch that could take the
total past the budget is refused.

Needs the OpenStack CLI (`pip install python-openstackclient`) and an
application credential in the environment (OS_APPLICATION_CREDENTIAL_ID,
OS_APPLICATION_CREDENTIAL_SECRET).

  box.py launch [--hours 3] [--flavor a10-45]   start an instance
  box.py run <script> [--no-wait]               run a script there, stream its log
  box.py put <file> <object> / get <object> <file>
  box.py status                                 heartbeat and servers
  box.py delete                                 delete every box instance now
  box.py ledger                                 settle the ledger, print the spend
"""

import argparse
import json
import math
import os
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
LEDGER = os.environ.get("BOX_LEDGER", "/mnt/project-files/ovh-gpu/ledger.json")
COMPUTE_REGION = "GRA11"
SWIFT_REGION = "GRA"
CONTAINER = "gpuiav-box"
IMAGE = "41d52c27-b538-4e11-8f5e-346bec89f87a"  # Ubuntu 24.04 - NVIDIA - v580
NETWORK = "bcf59eb2-9d83-41cc-b4f5-0435ed594833"  # Ext-Net
# Rupees an hour, 18% GST included; OVH bills every hour started. Only
# a10-45 is a known INR price; the rest are scaled from OVH's EUR list
# (a10-45 0.76, l4-90 0.75, t2-45 0.80, l40s-90 1.40) and rounded up.
RATES = {"a10-45": 70 * 1.18, "a10-90": 140 * 1.18, "l4-90": 70 * 1.18,
         "t2-45": 74 * 1.18, "l40s-90": 130 * 1.18}
BUDGET = float(os.environ.get("BOX_BUDGET_INR", "12000"))
MAX_HOURS = 8
PREFIX = "gpuiav-box-"


def openstack(region, *args, parse=True):
    env = dict(os.environ, OS_REGION_NAME=region, OS_AUTH_URL="https://auth.cloud.ovh.net/v3")
    result = subprocess.run(["openstack", *args] + (["-f", "json"] if parse else []),
                            capture_output=True, text=True, env=env)
    if result.returncode != 0:
        raise SystemExit(f"openstack {' '.join(args)}: {result.stderr.strip()}")
    return json.loads(result.stdout) if parse and result.stdout.strip() else result.stdout


def swift(*args, parse=False):
    return openstack(SWIFT_REGION, "object", *args, parse=parse)


def load_ledger():
    if os.path.exists(LEDGER):
        with open(LEDGER) as f:
            return json.load(f)
    return {"budget_inr": BUDGET, "servers": []}


def save_ledger(ledger):
    os.makedirs(os.path.dirname(LEDGER), exist_ok=True)
    with open(LEDGER + ".tmp", "w") as f:
        json.dump(ledger, f, indent=1)
    os.replace(LEDGER + ".tmp", LEDGER)


def cost(entry, now):
    if entry.get("failed"):
        return 0  # never built (no host free); OVH does not bill these
    end = entry.get("deleted") or now
    hours = max(1, math.ceil((end - entry["created"]) / 3600 - 1e-9))
    return hours * RATES[entry["flavor"]]


def settle(ledger):
    """Marks servers that no longer exist as deleted, and returns the spend."""
    now = time.time()
    live = {s["ID"]: s for s in openstack(COMPUTE_REGION, "server", "list")}
    for entry in ledger["servers"]:
        if not entry.get("deleted") and entry["id"] not in live:
            entry["deleted"] = now
    save_ledger(ledger)
    return sum(cost(e, now) for e in ledger["servers"]), live


def write_user_data(deadline, hours, idle):
    env = "\n".join([
        f"OS_APPLICATION_CREDENTIAL_ID={os.environ['OS_APPLICATION_CREDENTIAL_ID']}",
        f"OS_APPLICATION_CREDENTIAL_SECRET={os.environ['OS_APPLICATION_CREDENTIAL_SECRET']}",
        f"BOX_CONTAINER={CONTAINER}", f"BOX_DEADLINE={deadline}",
        f"BOX_IDLE_MINUTES={idle}",
        f"BOX_SWIFT_REGION={SWIFT_REGION}", f"BOX_COMPUTE_REGION={COMPUTE_REGION}"])
    agent = open(os.path.join(HERE, "agent.py")).read()
    backstop = int(hours * 3600 + 300)
    user_data = "#cloud-config\n" + json.dumps({
        "write_files": [
            {"path": "/root/ovh.env", "permissions": "0600", "content": env + "\n"},
            {"path": "/root/agent.py", "permissions": "0700", "content": agent},
        ],
        "runcmd": [
            ["systemd-run", f"--on-active={backstop}", "--unit=box-backstop",
             "python3", "/root/agent.py", "kill"],
            ["systemd-run", "--unit=box-agent", "-p", "Restart=always",
             "python3", "/root/agent.py", "serve"],
        ],
    })
    with tempfile.NamedTemporaryFile("w", suffix=".yaml", delete=False) as f:
        f.write(user_data)
        return f.name


def launch(args):
    ledger = load_ledger()
    spent, live = settle(ledger)
    ours = [s for s in live.values() if s["Name"].startswith(PREFIX)]
    if ours:
        raise SystemExit(f"already running: {[s['Name'] for s in ours]}")
    hours = min(args.hours, MAX_HOURS)
    worst = spent + math.ceil(hours + 0.5) * RATES[args.flavor]
    if worst > BUDGET:
        raise SystemExit(f"refused: spent {spent:.0f} INR, this run could reach {worst:.0f}, "
                         f"budget {BUDGET:.0f}")
    openstack(SWIFT_REGION, "container", "create", CONTAINER)
    for name in swift("list", CONTAINER, "-f", "value").split():
        if name.startswith(("cmd/", "out/", "status")):
            swift("delete", CONTAINER, name)
    give_up = time.time() + args.wait_capacity * 60
    while True:
        deadline = time.time() + hours * 3600
        path = write_user_data(deadline, hours, args.idle)
        name = PREFIX + time.strftime("%m%d-%H%M")
        try:
            server = openstack(COMPUTE_REGION, "server", "create", "--flavor", args.flavor,
                               "--image", IMAGE, "--network", NETWORK, "--user-data", path,
                               name)
        finally:
            os.unlink(path)
        for _ in range(90):
            server = openstack(COMPUTE_REGION, "server", "show", server["id"])
            if server.get("status") in ("ACTIVE", "ERROR"):
                break
            time.sleep(10)
        entry = {"id": server["id"], "name": name, "flavor": args.flavor,
                 "created": time.time(), "deadline": deadline, "purpose": args.purpose}
        ledger["servers"].append(entry)
        if server.get("status") == "ACTIVE":
            break
        # "No valid host was found": no free GPU in the region right now.
        entry["failed"] = True
        entry["deleted"] = time.time()
        openstack(COMPUTE_REGION, "server", "delete", server["id"], parse=False)
        save_ledger(ledger)
        print(f"{name} did not build ({server.get('fault', {}).get('message')}); ", end="")
        if time.time() > give_up:
            raise SystemExit("giving up")
        print("trying again in 5 minutes", flush=True)
        time.sleep(300)
    save_ledger(ledger)
    print(f"launched {name} {server['id']} ({args.flavor}, deadline in {hours} h); "
          f"spent so far {spent:.0f} of {BUDGET:.0f} INR")


def run(args):
    job = time.strftime("%H%M%S-") + os.path.basename(args.script).replace(".sh", "")
    swift("create", CONTAINER, args.script, "--name", f"cmd/{job}.sh")
    print(f"job {job}", flush=True)
    if args.no_wait:
        return
    tail(job)


def fetch(name):
    with tempfile.TemporaryDirectory() as directory:
        target = os.path.join(directory, "object")
        env = dict(os.environ, OS_REGION_NAME=SWIFT_REGION,
                   OS_AUTH_URL="https://auth.cloud.ovh.net/v3")
        result = subprocess.run(["openstack", "object", "save", "--file", target,
                                 CONTAINER, name], capture_output=True, env=env)
        if result.returncode != 0:
            return None
        return open(target, "rb").read()


def tail(job):
    shown = 0
    while True:
        log = fetch(f"out/{job}.log") or b""
        if len(log) > shown:
            sys.stdout.write(log[shown:].decode(errors="replace"))
            sys.stdout.flush()
            shown = len(log)
        status = fetch(f"out/{job}.exit")
        if status is not None:
            log = fetch(f"out/{job}.log") or b""
            sys.stdout.write(log[shown:].decode(errors="replace"))
            print(f"\n[exit {status.decode()}]")
            return
        time.sleep(10)


def main():
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("launch")
    p.add_argument("--hours", type=float, default=3)
    p.add_argument("--flavor", default="a10-45")
    p.add_argument("--idle", type=float, default=20, help="minutes idle before self-delete")
    p.add_argument("--purpose", default="")
    p.add_argument("--wait-capacity", type=float, default=0,
                   help="minutes to keep retrying while the region has no free GPU")
    p = sub.add_parser("run")
    p.add_argument("script")
    p.add_argument("--no-wait", action="store_true")
    p = sub.add_parser("tail")
    p.add_argument("job")
    p = sub.add_parser("put")
    p.add_argument("file")
    p.add_argument("object")
    p = sub.add_parser("get")
    p.add_argument("object")
    p.add_argument("file")
    sub.add_parser("status")
    sub.add_parser("delete")
    sub.add_parser("ledger")
    args = parser.parse_args()
    if args.command == "launch":
        launch(args)
    elif args.command == "run":
        run(args)
    elif args.command == "tail":
        tail(args.job)
    elif args.command == "put":
        swift("create", CONTAINER, args.file, "--name", args.object)
    elif args.command == "get":
        data = fetch(args.object)
        if data is None:
            raise SystemExit(f"no object {args.object}")
        open(args.file, "wb").write(data)
    elif args.command == "status":
        data = fetch("status")
        print(data.decode() if data else "no heartbeat")
        for s in openstack(COMPUTE_REGION, "server", "list"):
            print(s["Name"], s["ID"], s["Status"], s.get("Networks"))
    elif args.command == "delete":
        for s in openstack(COMPUTE_REGION, "server", "list"):
            if s["Name"].startswith(PREFIX):
                openstack(COMPUTE_REGION, "server", "delete", s["ID"], parse=False)
                print("deleted", s["Name"])
        settle(load_ledger())
    elif args.command == "ledger":
        ledger = load_ledger()
        spent, _ = settle(ledger)
        for e in ledger["servers"]:
            hours = ((e.get("deleted") or time.time()) - e["created"]) / 3600
            print(f"{e['name']:24} {e['flavor']:8} {hours:5.2f} h  {cost(e, time.time()):6.0f} INR"
                  f"  {'running' if not e.get('deleted') else ''} {e.get('purpose', '')}")
        print(f"total {spent:.0f} of {BUDGET:.0f} INR")


if __name__ == "__main__":
    main()
