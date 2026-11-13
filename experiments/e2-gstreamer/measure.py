#!/usr/bin/env python3
"""E2: what each step of the GStreamer backend's video path costs per frame.

Runs the steps the backend runs, without a window and as fast as the
machine goes (no clock), and prints CPU milliseconds per frame:

  decode   the decoder alone (its own output format, usually I420 or NV12)
  convert  decode, then videoconvert to BGRA on one thread
  convertN decode, then videoconvert to BGRA on every core (the backend's
           setting)

The backend's own copy into a GPUI image, and GPUI's upload, are measured
by running the lab with GPUI_AV_GSTREAMER_STATS=1 (spec 4.2, E2).

usage: measure.py <clip>... (needs gst-launch-1.0)
"""

import os
import resource
import subprocess
import sys
import time

STEPS = {
    "decode": "fakesink sync=false",
    "convert": "videoconvert n-threads=1 ! video/x-raw,format=BGRA ! fakesink sync=false",
    "convertN": "videoconvert n-threads=0 ! video/x-raw,format=BGRA ! fakesink sync=false",
}


def frames(clip):
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-count_packets", "-select_streams", "v:0",
         "-show_entries", "stream=nb_read_packets", "-of", "csv=p=0", clip],
        capture_output=True, text=True, check=True)
    return int(out.stdout.strip())


def run(clip, step):
    uri = "file://" + os.path.abspath(clip)
    command = ["gst-launch-1.0", "-q", "uridecodebin", f"uri={uri}",
               "caps=video/x-raw", "!"] + STEPS[step].split()
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    started = time.monotonic()
    subprocess.run(command, check=True, stdout=subprocess.DEVNULL)
    wall = time.monotonic() - started
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    cpu = (after.ru_utime - before.ru_utime) + (after.ru_stime - before.ru_stime)
    return cpu, wall


def main():
    print(f"{'clip':34} {'step':9} {'frames':>6} {'cpu ms/frame':>13} {'wall ms/frame':>14}")
    for clip in sys.argv[1:]:
        count = frames(clip)
        for step in STEPS:
            # The best of three, so a busy moment does not count.
            cpu, wall = min(run(clip, step) for _ in range(3))
            print(f"{os.path.basename(clip):34} {step:9} {count:6} "
                  f"{cpu * 1000 / count:13.2f} {wall * 1000 / count:14.2f}")


if __name__ == "__main__":
    main()
