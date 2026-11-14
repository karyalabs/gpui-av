#!/usr/bin/env bash
# Why the window holds at 30 fps on Xorg with the A10: the virtual screen's
# only CRTC runs 1024x768 at 85 Hz and the window sits partly outside it.
# Same bench with the CRTC at 2560x1440 60 Hz, then at 1920x1080.
cd /root/run
export DISPLAY=:0
OUT=/root/run/out/refresh; mkdir -p $OUT
python3 - /tmp/one.json <<'P'
import json, sys
m = json.load(open("corpus/tier-a.json"))
m["clips"] = [c for c in m["clips"] if c["file"] == "h264-1920x1080-60fps.mp4"]
m["clips"][0]["play_seconds"] = 15
json.dump(m, open(sys.argv[1], "w"))
P
xrandr --verbose | grep -E 'connected|\*current' | head
for mode in "2560x1440 60" "1920x1080 60"; do
  set -- $mode
  xrandr --output DVI-D-0 --mode $1 --rate $2 --pos 0x0 2>&1
  xrandr | grep '\*'
  for lab in gpui-av-lab lab-nv12; do
    [ -x $lab ] || continue
    GPUI_AV_GSTREAMER_STATS=1 timeout 60 ./$lab --bench /tmp/one.json --clips clips --backend gstreamer --out $OUT/$1-$lab.json 2>&1 | grep -E 'gpui-av|bench:'
    python3 -c "import json;r=json.load(open('$OUT/$1-$lab.json'))['results'][0];print('$1 $lab', r['window_fps'], r['video_fps'], r['dropped_percent'], r['cpu_percent'])"
  done
done
for vm in 0 1; do
  echo "vblank sync env __GL_SYNC_TO_VBLANK=$vm"
  __GL_SYNC_TO_VBLANK=$vm timeout 60 ./gpui-av-lab --bench /tmp/one.json --clips clips --backend gstreamer --out $OUT/vblank$vm.json >/dev/null 2>&1
  python3 -c "import json;r=json.load(open('$OUT/vblank$vm.json'))['results'][0];print(r['window_fps'], r['dropped_percent'])"
done
