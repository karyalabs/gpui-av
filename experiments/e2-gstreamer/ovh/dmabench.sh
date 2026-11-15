#!/usr/bin/env bash
# E5 step 2 on an OVH NVIDIA GPU: GStreamer's VA-API decoders (nvidia-vaapi-driver)
# handing dmabufs to our GPUI, against the same decoders through NV12 in
# memory. Tier A, the heavier clips and a 10 minute 1080p60 soak, muted.
A=/root/agent.py
cd /root/run; export DISPLAY=:0
[ -x lab-dmabuf ] || { python3 $A get files/lab-dmabuf lab-dmabuf && chmod +x lab-dmabuf; }
# Ubuntu 24.04 ships nvidia-vaapi-driver 0.0.8, whose direct backend cannot
# open the 580 driver; its EGL backend works. GStreamer only uses VA drivers
# on its allowlist (Intel, Mesa) unless GST_VA_ALL_DRIVERS is set.
export LIBVA_DRIVER_NAME=nvidia NVD_BACKEND=egl GST_VA_ALL_DRIVERS=1
# VA-API decoders first: NVDEC's own decoders give CUDA memory, not dmabufs.
export GST_PLUGIN_FEATURE_RANK=nvh264dec:0,nvh265dec:0,nvav1dec:0,nvvp9dec:0,nvvp8dec:0,vah264dec:300,vah265dec:300,vaav1dec:300,vavp9dec:300,vavp8dec:300
rm -rf ~/.cache/gstreamer-1.0
gst-inspect-1.0 vah264dec >/dev/null 2>&1 || { echo "no VA decoders in GStreamer"; gst-inspect-1.0 va; exit 1; }
timeout 30 gst-launch-1.0 -v filesrc location=clips/h264-1920x1080-60fps.mp4 ! qtdemux ! h264parse ! vah264dec ! 'video/x-raw(memory:DMABuf)' ! fakesink num-buffers=60 2>&1 | grep -m2 -E 'drm-format|ERROR'
OUT=/root/run/out/dmabuf; mkdir -p $OUT
gpu_log() { nvidia-smi --query-gpu=timestamp,utilization.gpu,utilization.decoder,memory.used --format=csv -l 1 >"$1" 2>&1 & echo $!; }
for mode in dmabuf nv12; do
  if [ $mode = dmabuf ]; then export GPUI_AV_DMABUF=1; else unset GPUI_AV_DMABUF; fi
  for set in tier-a:corpus/tier-a.json heavy:gpu-clips.json; do
    name=${set%%:*}; manifest=${set#*:}
    echo "== $name, VA-API, $mode"
    pid=$(gpu_log $OUT/gpu-$name-$mode.csv)
    GPUI_AV_GSTREAMER_STATS=1 timeout 900 ./lab-dmabuf --bench $manifest --clips clips --backend gstreamer --out $OUT/$name-$mode.json >$OUT/$name-$mode.log 2>&1
    echo "exit $?"; kill $pid
    grep -E 'gpui-av-gstreamer|bench:|cannot import|  ' $OUT/$name-$mode.log | grep -v ALSA | head -40
  done
  echo "== soak, VA-API, $mode"
  pid=$(gpu_log $OUT/gpu-soak-$mode.csv)
  GPUI_AV_GSTREAMER_STATS=1 timeout 900 ./lab-dmabuf --soak 600 --backend gstreamer clips/h264-1920x1080-60fps.mp4 --out $OUT/soak-$mode.json >$OUT/soak-$mode.log 2>&1
  echo "exit $?"; kill $pid; grep -E 'gpui-av-gstreamer|cannot import' $OUT/soak-$mode.log
done
python3 - <<'P'
import json
for f in ["tier-a-dmabuf","tier-a-nv12","heavy-dmabuf","heavy-nv12"]:
    try: d=json.load(open(f"/root/run/out/dmabuf/{f}.json"))
    except Exception as e: print(f, e); continue
    print("##", f, d["passed"], "passed")
    for r in d["results"]:
        print(r["clip"][:32], r["outcome"], r.get("cpu_percent"), r.get("dropped_percent"), r.get("video_fps"))
P
cd /root/run/out && tar -czf /root/results-dmabuf.tar.gz dmabuf && python3 $A put /root/results-dmabuf.tar.gz files/results-dmabuf.tar.gz && echo uploaded
