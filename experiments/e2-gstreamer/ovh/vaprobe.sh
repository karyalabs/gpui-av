#!/usr/bin/env bash
# Can GStreamer's VA decoders run on the A10 (nvidia-vaapi-driver) and hand
# out dmabufs? Then waits up to 50 minutes for files/lab-dmabuf, so the box
# stays up while the lab is built.
cd /root/run
ls -l /dev/dri/ 2>&1
export LIBVA_DRIVER_NAME=nvidia NVD_BACKEND=direct
vainfo 2>&1 | tail -8
rm -rf ~/.cache/gstreamer-1.0
gst-inspect-1.0 va 2>&1 | grep -E 'dec|Version'
gst-inspect-1.0 vah264dec 2>&1 | sed -n '/SRC template/,/Element Flags/p' | head -30
for dec in vah264dec vah265dec vaav1dec vavp9dec; do
  echo "== $dec dmabuf"
done
clip=clips/h264-1920x1080-60fps.mp4
GST_DEBUG=va*:3 timeout 30 gst-launch-1.0 -v filesrc location=$clip ! qtdemux ! h264parse ! vah264dec ! 'video/x-raw(memory:DMABuf)' ! fakesink num-buffers=60 2>&1 | grep -E 'caps|ERROR|WARN|drm-format' | head -20
echo "== playbin-style autoplug with nv ranked out"
GST_PLUGIN_FEATURE_RANK=nvh264dec:0,nvh265dec:0,nvav1dec:0,nvvp9dec:0,vah264dec:300,vah265dec:300,vaav1dec:300,vavp9dec:300 timeout 30 gst-launch-1.0 -v uridecodebin uri=file://$PWD/$clip caps='video/x-raw(memory:DMABuf)' ! fakesink num-buffers=60 2>&1 | grep -E 'drm-format|ERROR|vah264' | head -10
A=/root/agent.py
for i in $(seq 300); do python3 $A get files/lab-dmabuf lab-dmabuf 2>/dev/null && break; sleep 10; done
echo "lab arrived"
