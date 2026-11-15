#!/usr/bin/env bash
# Second VA probe: the direct backend failed, so try nvidia-vaapi-driver's
# EGL backend on the X display, and log why direct fails.
cd /root/run
export DISPLAY=:0 LIBVA_DRIVER_NAME=nvidia
cat /sys/module/nvidia_drm/parameters/modeset 2>&1
dpkg -l nvidia-vaapi-driver | tail -1
echo "== direct"
NVD_BACKEND=direct NVD_LOG=1 vainfo 2>&1 | grep -viE 'VAProfile' | tail -15
echo "== egl"
NVD_BACKEND=egl vainfo 2>&1 | tail -4
for backend in egl direct; do
  export NVD_BACKEND=$backend
  rm -rf ~/.cache/gstreamer-1.0
  echo "== gst $backend"
  gst-inspect-1.0 va 2>&1 | grep -E 'dec'
  clip=clips/h264-1920x1080-60fps.mp4
  timeout 30 gst-launch-1.0 -v filesrc location=$clip ! qtdemux ! h264parse ! vah264dec ! 'video/x-raw(memory:DMABuf)' ! fakesink num-buffers=60 2>&1 | grep -E 'drm-format|ERROR|WARN|Got EOS' | head -8
done
