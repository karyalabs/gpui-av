#!/usr/bin/env bash
# Generates the test clips the experiments use (video files are never
# committed). Needs ffmpeg with libx264. Usage: scripts/make-clips.sh [dir]
set -euo pipefail
OUT="${1:-clips}"
mkdir -p "$OUT"
for size in 1280x720 1920x1080; do
  ffmpeg -loglevel error -y \
    -f lavfi -i "testsrc2=size=$size:rate=30" \
    -f lavfi -i "sine=frequency=440" \
    -t 20 -c:v libx264 -pix_fmt yuv420p -c:a aac -shortest \
    "$OUT/h264-$size.mp4"
done
ls -l "$OUT"
