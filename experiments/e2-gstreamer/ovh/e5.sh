#!/usr/bin/env bash
# Run 3 (E5): the same bench and soak as baseline.sh, with the lab built on
# Karya Labs' GPUI with NV12 surfaces (`--features karya-gpui-linux`): frames
# go from the decoder to the GPU with no CPU conversion or copy.
set -uo pipefail
cd /root/run
[ -x lab-nv12 ] || { python3 /root/agent.py get files/lab-nv12 lab-nv12 && chmod +x lab-nv12; }
python3 /root/agent.py get files/baseline.sh /root/run/baseline.sh && \
  RUN_NAME=nv12 LAB=./lab-nv12 SKIP_MEASURE=1 bash /root/run/baseline.sh
