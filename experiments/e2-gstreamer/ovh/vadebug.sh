#!/usr/bin/env bash
# Why do the VA decoders give the lab no frames on the L4?
cd /root/run; export DISPLAY=:0 LIBVA_DRIVER_NAME=nvidia NVD_BACKEND=egl GST_VA_ALL_DRIVERS=1
clip=clips/h264-1920x1080-60fps.mp4
gst-inspect-1.0 vah264dec | sed -n '/SRC template/,/Element Flags/p' | head -30
for caps in 'video/x-raw' 'video/x-raw(memory:DMABuf)' 'video/x-raw(memory:VAMemory)'; do
  echo "== $caps"
  GST_DEBUG=2,va*:4 timeout 20 gst-launch-1.0 -v filesrc location=$clip ! qtdemux ! h264parse ! vah264dec ! "$caps" ! fakesink num-buffers=30 2>&1 | grep -vE 'ALSA|pw.conf' | grep -E 'caps =|ERROR|WARN|EOS|Execution ended' | sort | uniq | head -14
done
echo "== VAMemory -> vapostproc -> DMABuf"
GST_DEBUG=2 timeout 20 gst-launch-1.0 -v filesrc location=$clip ! qtdemux ! h264parse ! vah264dec ! vapostproc ! 'video/x-raw(memory:DMABuf)' ! fakesink num-buffers=30 2>&1 | grep -E 'caps =|ERROR|WARN|Execution ended' | sort | uniq | head -10
NVD_LOG=1 timeout 20 gst-launch-1.0 filesrc location=$clip ! qtdemux ! h264parse ! vah264dec ! 'video/x-raw(memory:DMABuf)' ! fakesink num-buffers=5 2>&1 | grep -iE 'export|error|fail' | head -10
