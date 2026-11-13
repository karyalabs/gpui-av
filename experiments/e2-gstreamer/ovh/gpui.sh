#!/usr/bin/env bash
# Run 2: GPUI itself on a Linux GPU. Which adapter wgpu picks and whether a
# window draws, on Xorg with the NVIDIA driver, on Xvfb (an X server the
# NVIDIA Vulkan driver may not present to) and on Weston (Wayland,
# headless), with the stock GPUI and with karyalabs/zed#5 (skip adapters
# that cannot present). Screenshots of each window go into the results.
set -uo pipefail
A=/root/agent.py
cd /root/run
OUT=/root/run/out/gpui
mkdir -p "$OUT"
[ -x lab-zed5 ] || { python3 $A get files/lab-zed5 lab-zed5 && chmod +x lab-zed5; }
CLIP=clips/h264-1920x1080-60fps.mp4

pgrep -x Xvfb >/dev/null || { nohup Xvfb :99 -screen 0 2560x1440x24 >/tmp/xvfb.log 2>&1 & sleep 2; }

# One windowed bench of the 1080p60 clip, plus a screenshot after 4 s.
try() { # <name> <lab> <env...>
  local name=$1 lab=$2; shift 2
  echo "== $name"
  local manifest=/tmp/one.json
  python3 - "$manifest" <<'EOF'
import json, sys
m = json.load(open("corpus/tier-a.json"))
m["clips"] = [c for c in m["clips"] if c["file"] == "h264-1920x1080-60fps.mp4"]
m["clips"][0]["play_seconds"] = 20
json.dump(m, open(sys.argv[1], "w"))
EOF
  env "$@" RUST_LOG=wgpu_core=info,wgpu_hal=warn,gpui_wgpu=info,gpui=info GPUI_AV_GSTREAMER_STATS=1 \
    timeout 120 "./$lab" --bench "$manifest" --clips clips --backend gstreamer --out "$OUT/$name.json" \
    >"$OUT/$name.log" 2>&1 &
  local pid=$!
  sleep 8
  local display
  display=$(env "$@" sh -c 'echo $DISPLAY')
  if [ -n "$display" ]; then
    ffmpeg -loglevel error -y -f x11grab -video_size 2560x1440 -i "$display" -frames:v 1 "$OUT/$name.png" || true
  fi
  wait $pid; echo "exit $?"
  grep -iE 'adapter|panic|error|backend|vulkan|gl ' "$OUT/$name.log" | head -15
  tail -5 "$OUT/$name.log"
}

for lab in gpui-av-lab lab-zed5; do
  try "xorg-nvidia-$lab" $lab DISPLAY=:0
  try "xvfb-$lab" $lab DISPLAY=:99
  try "xvfb-llvmpipe-$lab" $lab DISPLAY=:99 VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json
done

echo "== weston (wayland, headless)"
export XDG_RUNTIME_DIR=/tmp/xdg; mkdir -p $XDG_RUNTIME_DIR; chmod 700 $XDG_RUNTIME_DIR
nohup weston --backend=headless --renderer=gl --width=2560 --height=1440 --socket=wl-gpu >/tmp/weston-gl.log 2>&1 &
sleep 4
if [ -S $XDG_RUNTIME_DIR/wl-gpu ]; then
  for lab in gpui-av-lab lab-zed5; do
    try "weston-gl-$lab" $lab -u DISPLAY WAYLAND_DISPLAY=wl-gpu
  done
else
  echo "weston gl renderer failed:"; tail -20 /tmp/weston-gl.log
  nohup weston --backend=headless --width=2560 --height=1440 --socket=wl-sw >/tmp/weston-sw.log 2>&1 &
  sleep 4
  for lab in gpui-av-lab lab-zed5; do
    try "weston-sw-$lab" $lab -u DISPLAY WAYLAND_DISPLAY=wl-sw
  done
fi
pkill weston

echo "== vulkan devices"
DISPLAY=:0 vulkaninfo --summary 2>&1 | grep -E 'GPU[0-9]|deviceName|driverName' | tee "$OUT/vulkan-xorg.txt"

cd /root/run/out && tar -czf /root/results-gpui.tar.gz gpui && python3 $A put /root/results-gpui.tar.gz files/results-gpui.tar.gz
echo "uploaded results"
