#!/usr/bin/env bash
# Is the 30 fps cap the FIFO present mode? Waits (up to 40 min) for a lab
# built with GPUI_WGPU_PRESENT_MODE, then benches each mode on Xorg.
A=/root/agent.py
cd /root/run; export DISPLAY=:0
for i in $(seq 240); do python3 $A get files/lab-present lab-present 2>/dev/null && break; sleep 10; done
chmod +x lab-present || exit 1
OUT=/root/run/out/present; mkdir -p $OUT
for mode in fifo mailbox immediate fifo_relaxed; do
  GPUI_WGPU_PRESENT_MODE=$mode GPUI_AV_GSTREAMER_STATS=1 timeout 60 ./lab-present --bench /tmp/one.json --clips clips --backend gstreamer --out $OUT/$mode.json 2>&1 | grep -E 'gpui-av|bench:|present'
  python3 -c "import json;r=json.load(open('$OUT/$mode.json'))['results'][0];print('$mode', r['window_fps'], r['video_fps'], r['dropped_percent'], r['cpu_percent'])"
done
