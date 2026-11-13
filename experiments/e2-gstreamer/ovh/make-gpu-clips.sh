#!/usr/bin/env bash
# The heavier clips listed in gpu-clips.json (never committed). Needs ffmpeg
# with libx264, libx265, libsvtav1 and libvpx. Usage: make-gpu-clips.sh [dir]
set -euo pipefail
OUT="${1:-clips}"
mkdir -p "$OUT"
FF=(ffmpeg -loglevel error -y)
pattern() { echo -f lavfi -i "testsrc2=size=$1:rate=$2"; }
# shellcheck disable=SC2046
{
"${FF[@]}" $(pattern 3840x2160 30) -t 20 -c:v libx264 -preset veryfast -pix_fmt yuv420p "$OUT/h264-3840x2160.mp4"
"${FF[@]}" $(pattern 3840x2160 60) -t 20 -c:v libx265 -preset ultrafast -pix_fmt yuv420p10le \
  -x265-params log-level=error "$OUT/h265-10bit-3840x2160-60fps.mp4"
"${FF[@]}" $(pattern 3840x2160 30) -t 10 -c:v libsvtav1 -preset 12 -pix_fmt yuv420p10le "$OUT/av1-10bit-3840x2160.mp4"
"${FF[@]}" $(pattern 1920x1080 60) -t 30 -c:v libsvtav1 -preset 12 "$OUT/av1-1920x1080-60fps.mp4"
"${FF[@]}" $(pattern 1920x1080 60) -t 30 -c:v libvpx-vp9 -deadline realtime -cpu-used 8 -row-mt 1 \
  "$OUT/vp9-1920x1080-60fps.webm"
}
