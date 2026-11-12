#!/usr/bin/env bash
# Generates the test clips the experiments use (video files are never
# committed). Needs ffmpeg with libx264. Usage: scripts/make-clips.sh [dir]
set -euo pipefail
OUT="${1:-clips}"
mkdir -p "$OUT"

# clip <name> <size> <rate> <seconds> [extra ffmpeg output options...]
clip() {
  local name="$1" size="$2" rate="$3" seconds="$4"
  shift 4
  ffmpeg -loglevel error -y \
    -f lavfi -i "testsrc2=size=$size:rate=$rate" \
    -f lavfi -i "sine=frequency=440" \
    -t "$seconds" -c:v libx264 -pix_fmt yuv420p \
    -color_range tv -colorspace bt709 -color_primaries bt709 -color_trc bt709 \
    -c:a aac -shortest -movflags +faststart "$@" \
    "$OUT/$name.mp4"
}

clip h264-1280x720 1280x720 30 20
clip h264-1920x1080 1920x1080 30 20
# The smoothness check (spec 5.2, criterion 1): 1080p60 for 60 seconds.
clip h264-1920x1080-60fps 1920x1080 60 60
# Portrait from a phone: stored landscape with a rotation tag of 90 degrees
# clockwise (ffmpeg counts counter-clockwise, hence -90).
clip h264-1280x720-unrotated 1280x720 30 10
ffmpeg -loglevel error -y -display_rotation:v:0 -90 -i "$OUT/h264-1280x720-unrotated.mp4" \
  -c copy -movflags +faststart "$OUT/h264-1280x720-rotate90.mp4"
rm "$OUT/h264-1280x720-unrotated.mp4"
ls -l "$OUT"
