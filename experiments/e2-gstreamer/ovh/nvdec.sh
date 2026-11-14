#!/usr/bin/env bash
# Run 1b after a setup re-run: the X server on the GPU, then the NVDEC half
# of the baseline (the software half ran already).
A=/root/agent.py
python3 $A get files/setup.sh /root/setup.sh && SKIP_BUNDLE=1 bash /root/setup.sh 2>&1 | sed -n '/== X/,$p'
python3 $A get files/baseline.sh /root/run/baseline.sh
RUN_NAME=nvdec MODES=default bash /root/run/baseline.sh
