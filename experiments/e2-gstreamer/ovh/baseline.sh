#!/usr/bin/env bash
# Run 1: the E2 numbers on a real GPU. Tier A and the heavier clips through
# the lab (NVDEC as GStreamer picks it, then software only), a 10 minute
# 1080p60 soak, and measure.py's per-step costs. Everything is muted.
set -uo pipefail
A=/root/agent.py
cd /root/run
export DISPLAY=$(cat display)
OUT=/root/run/out/${RUN_NAME:-baseline}
mkdir -p "$OUT"
# Software decode only: every hardware decoder ranked out.
SOFTWARE="nvh264dec:0,nvh265dec:0,nvav1dec:0,nvvp9dec:0,nvvp8dec:0,nvmpeg2videodec:0,nvjpegdec:0,nvh264sldec:0,nvh265sldec:0,vah264dec:0,vah265dec:0,vaav1dec:0,vavp9dec:0"
LAB=${LAB:-./gpui-av-lab}

gpu_log() { # <file>: decoder and 3D load, once a second, until killed
  nvidia-smi --query-gpu=timestamp,utilization.gpu,utilization.decoder,memory.used,clocks.gr --format=csv -l 1 >"$1" 2>&1 &
  echo $!
}

decoders() { # which decoder uridecodebin picks for each clip
  for clip in "$@"; do
    picked=$(timeout 20 gst-launch-1.0 -v uridecodebin uri="file://$PWD/$clip" caps=video/x-raw ! fakesink num-buffers=5 2>&1 |
      grep -oE '(nv[a-z0-9]+dec|avdec_[a-z0-9]+|va[a-z0-9]+dec|vp[89]dec|dav1ddec|av1dec)[0-9]*' | sort -u | tr '\n' ' ')
    echo "$(basename "$clip"): $picked"
  done
}

echo "== decoder picked (default ranks)"
decoders clips/*.mp4 clips/*.mov clips/*.webm 2>/dev/null | tee "$OUT/decoders-default.txt"
echo "== decoder picked (software)"
GST_PLUGIN_FEATURE_RANK=$SOFTWARE decoders clips/*.mp4 clips/*.webm | tee "$OUT/decoders-software.txt"

for mode in ${MODES:-default software}; do
  if [ $mode = software ]; then export GST_PLUGIN_FEATURE_RANK=$SOFTWARE; else unset GST_PLUGIN_FEATURE_RANK; fi
  echo "== tier A bench, $mode decoders"
  pid=$(gpu_log "$OUT/gpu-tier-a-$mode.csv")
  GPUI_AV_GSTREAMER_STATS=1 timeout 900 $LAB --bench corpus/tier-a.json --clips clips --backend gstreamer \
    --out "$OUT/tier-a-$mode.json" >"$OUT/tier-a-$mode.log" 2>&1
  echo "exit $?"; kill $pid
  tail -40 "$OUT/tier-a-$mode.log"
  echo "== heavier clips, $mode decoders"
  pid=$(gpu_log "$OUT/gpu-heavy-$mode.csv")
  GPUI_AV_GSTREAMER_STATS=1 timeout 900 $LAB --bench gpu-clips.json --clips clips --backend gstreamer \
    --out "$OUT/heavy-$mode.json" >"$OUT/heavy-$mode.log" 2>&1
  echo "exit $?"; kill $pid
  tail -30 "$OUT/heavy-$mode.log"
done
unset GST_PLUGIN_FEATURE_RANK

if [ -z "${SKIP_MEASURE:-}" ]; then
echo "== measure.py, default decoders"
python3 measure.py clips/h264-1280x720.mp4 clips/h264-1920x1080.mp4 clips/h264-1920x1080-60fps.mp4 \
  clips/hevc-1920x1080.mp4 clips/h264-3840x2160.mp4 clips/h265-10bit-3840x2160-60fps.mp4 \
  clips/av1-10bit-3840x2160.mp4 2>&1 | tee "$OUT/measure-default.txt"
case " ${MODES:-default software} " in *" software "*)
echo "== measure.py, software decoders"
GST_PLUGIN_FEATURE_RANK=$SOFTWARE python3 measure.py clips/h264-1280x720.mp4 clips/h264-1920x1080.mp4 \
  clips/h264-1920x1080-60fps.mp4 clips/hevc-1920x1080.mp4 clips/h264-3840x2160.mp4 \
  clips/h265-10bit-3840x2160-60fps.mp4 clips/av1-10bit-3840x2160.mp4 2>&1 | tee "$OUT/measure-software.txt"
;; esac
fi

echo "== soak, 10 minutes of 1080p60, default decoders"
pid=$(gpu_log "$OUT/gpu-soak.csv")
GPUI_AV_GSTREAMER_STATS=1 timeout 900 $LAB --soak 600 --backend gstreamer clips/h264-1920x1080-60fps.mp4 \
  --out "$OUT/soak.json" >"$OUT/soak.log" 2>&1
echo "exit $?"; kill $pid
tail -30 "$OUT/soak.log"

cd /root/run/out && tar -czf /root/results.tar.gz "${RUN_NAME:-baseline}" && python3 $A put /root/results.tar.gz files/results-${RUN_NAME:-baseline}.tar.gz
echo "uploaded results"
