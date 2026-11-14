#!/usr/bin/env python3
"""Runs on an OVH GPU instance and takes its work from object storage.

The machine running `box.py` cannot reach the instance over SSH, so both
sides talk through one Swift container:

  cmd/<id>.sh     a shell script for the instance to run (box.py writes it)
  out/<id>.log    its output so far, refreshed every few seconds
  out/<id>.exit   its exit status, written when it ends
  status          a heartbeat: uptime, what is running, time left
  files/...       anything either side wants to hand over

The instance deletes itself: at its deadline, whatever is running, and
after IDLE_MINUTES with nothing to run. So a lost controller costs at most
the idle time, never an instance left on. Only the Python standard library
is used, so it starts before any package is installed.

usage on the instance:  agent.py serve
                        agent.py kill   (delete this instance now)
                        agent.py get <object> <file>
                        agent.py put <file> <object>
"""

import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

ENV_FILE = "/root/ovh.env"
WORK = "/root/box"


def load_env():
    env = {}
    with open(ENV_FILE) as f:
        for line in f:
            line = line.strip()
            if line and not line.startswith("#") and "=" in line:
                key, value = line.split("=", 1)
                env[key] = value
    return env


ENV = load_env()
CONTAINER = ENV["BOX_CONTAINER"]
DEADLINE = float(ENV["BOX_DEADLINE"])  # Unix time
IDLE_MINUTES = float(ENV.get("BOX_IDLE_MINUTES", "20"))


class Cloud:
    def __init__(self):
        self.token = None
        self.expires = 0

    def auth(self):
        if self.token and time.time() < self.expires - 600:
            return
        body = {"auth": {"identity": {"methods": ["application_credential"],
                "application_credential": {"id": ENV["OS_APPLICATION_CREDENTIAL_ID"],
                                           "secret": ENV["OS_APPLICATION_CREDENTIAL_SECRET"]}}}}
        request = urllib.request.Request(
            "https://auth.cloud.ovh.net/v3/auth/tokens", data=json.dumps(body).encode(),
            headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(request, timeout=60) as response:
            self.token = response.headers["X-Subject-Token"]
            catalog = json.load(response)["token"]["catalog"]
        self.expires = time.time() + 3600
        self.swift = endpoint(catalog, "object-store", ENV["BOX_SWIFT_REGION"])
        self.compute = endpoint(catalog, "compute", ENV["BOX_COMPUTE_REGION"])

    def call(self, method, url, data=None, headers=None):
        self.auth()
        all_headers = {"X-Auth-Token": self.token}
        all_headers.update(headers or {})
        request = urllib.request.Request(url, data=data, method=method, headers=all_headers)
        with urllib.request.urlopen(request, timeout=600) as response:
            return response.read()

    def object_url(self, name):
        return f"{self.swift}/{CONTAINER}/{name}"

    def put(self, name, data):
        self.auth()
        self.call("PUT", self.object_url(name), data=data)

    def put_file(self, path, name):
        self.auth()
        size = os.path.getsize(path)
        with open(path, "rb") as f:
            request = urllib.request.Request(
                self.object_url(name), data=f, method="PUT",
                headers={"X-Auth-Token": self.token, "Content-Length": str(size)})
            urllib.request.urlopen(request, timeout=3600).read()

    def get_file(self, name, path):
        self.auth()
        request = urllib.request.Request(self.object_url(name),
                                         headers={"X-Auth-Token": self.token})
        with urllib.request.urlopen(request, timeout=3600) as response, open(path, "wb") as f:
            while chunk := response.read(1 << 20):
                f.write(chunk)

    def list(self, prefix):
        self.auth()
        names = self.call("GET", f"{self.swift}/{CONTAINER}?prefix={prefix}&format=json")
        return [entry["name"] for entry in json.loads(names)]

    def delete_self(self):
        with urllib.request.urlopen("http://169.254.169.254/openstack/latest/meta_data.json",
                                    timeout=30) as response:
            server = json.load(response)["uuid"]
        self.call("DELETE", f"{self.compute}/servers/{server}")


def endpoint(catalog, kind, region):
    for service in catalog:
        if service["type"] == kind:
            for entry in service["endpoints"]:
                if entry["interface"] == "public" and entry["region"] == region:
                    return entry["url"]
    raise RuntimeError(f"no {kind} endpoint in {region}")


def log(message):
    print(time.strftime("%H:%M:%S"), message, flush=True)


def serve():
    cloud = Cloud()
    os.makedirs(WORK, exist_ok=True)
    done = set()
    last_activity = time.time()
    last_beat = 0
    while True:
        now = time.time()
        if now >= DEADLINE:
            log("deadline reached, deleting this instance")
            break
        if now - last_activity > IDLE_MINUTES * 60:
            log("idle too long, deleting this instance")
            break
        try:
            if now - last_beat > 60:
                cloud.put("status", json.dumps({
                    "time": now, "uptime": float(open("/proc/uptime").read().split()[0]),
                    "seconds_left": DEADLINE - now,
                    "idle_seconds": now - last_activity}).encode())
                last_beat = now
            pending = sorted(n for n in cloud.list("cmd/") if n.endswith(".sh") and n not in done)
        except Exception as error:  # a network blip must not kill the loop
            log(f"poll failed: {error}")
            time.sleep(15)
            continue
        if not pending:
            time.sleep(5)
            continue
        name = pending[0]
        done.add(name)
        run(cloud, name)
        last_activity = time.time()
    for attempt in range(10):
        try:
            cloud.put("status", json.dumps({"time": time.time(), "deleting": True}).encode())
            cloud.delete_self()
            return
        except Exception as error:
            log(f"self-delete failed: {error}")
            time.sleep(30)
    # Last resort: at least stop burning GPU time if the API is unreachable.
    subprocess.run(["poweroff"])


def run(cloud, name):
    job = os.path.basename(name)[:-3]
    script = os.path.join(WORK, job + ".sh")
    output = os.path.join(WORK, job + ".log")
    cloud.get_file(name, script)
    log(f"running {job}")
    with open(output, "wb") as out:
        process = subprocess.Popen(["bash", script], stdout=out, stderr=subprocess.STDOUT,
                                   cwd=WORK, start_new_session=True)
        while True:
            try:
                status = process.wait(timeout=10)
                break
            except subprocess.TimeoutExpired:
                pass
            if time.time() >= DEADLINE:
                process.kill()
                status = -9
                break
            try:
                cloud.put_file(output, f"out/{job}.log")
            except Exception as error:
                log(f"log upload failed: {error}")
    for attempt in range(5):
        try:
            cloud.put_file(output, f"out/{job}.log")
            cloud.put(f"out/{job}.exit", str(status).encode())
            break
        except Exception as error:
            log(f"result upload failed: {error}")
            time.sleep(10)
    log(f"{job} exited {status}")


def main():
    if len(sys.argv) >= 2 and sys.argv[1] == "serve":
        serve()
    elif len(sys.argv) == 2 and sys.argv[1] == "kill":
        # The backstop timer: deletes the instance even if `serve` died.
        Cloud().delete_self()
    elif len(sys.argv) == 4 and sys.argv[1] == "get":
        Cloud().get_file(sys.argv[2], sys.argv[3])
    elif len(sys.argv) == 4 and sys.argv[1] == "put":
        Cloud().put_file(sys.argv[2], sys.argv[3])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
