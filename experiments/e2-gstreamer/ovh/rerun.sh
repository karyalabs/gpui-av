#!/usr/bin/env bash
# Run 4: runs 1 and 3 again with the lab's window no longer held to 30 fps
# while GPUI thinks it is inactive (always, on Xorg without a window
# manager). Image path and NV12 path, NVDEC and software decode.
A=/root/agent.py
cd /root/run
for lab in lab-stock2 lab-nv12b; do python3 $A get files/$lab $lab && chmod +x $lab; done
python3 $A get files/baseline.sh baseline.sh
RUN_NAME=image2 LAB=./lab-stock2 SKIP_MEASURE=1 bash baseline.sh
RUN_NAME=nv12b LAB=./lab-nv12b SKIP_MEASURE=1 bash baseline.sh
