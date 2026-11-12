#!/usr/bin/env bash
# Generates the synthetic tier A corpus (video files are never committed).
# The clips and what each must report are listed in corpus/tier-a.json;
# keep the two in step. Needs ffmpeg with libx264 and an HEVC encoder
# (libx265, or VideoToolbox on a Mac). Usage: scripts/make-clips.sh [dir]
set -euo pipefail
OUT="${1:-clips}"
mkdir -p "$OUT"

FF=(ffmpeg -loglevel error -y)
# The sound is a quiet 440 Hz tone: enough to prove audio plays, not loud
# enough to give anyone a headache during a long run.
TONE=(-f lavfi -i "sine=frequency=440")
AUDIO=(-af volume=0.05 -c:a aac)
H264=(-c:v libx264 -pix_fmt yuv420p)
BT709=(-color_range tv -colorspace bt709 -color_primaries bt709 -color_trc bt709)

picture() { # <size> <rate>
  echo -f lavfi -i "testsrc2=size=$1:rate=$2"
}

# clip <file> <size> <rate> <seconds> [extra ffmpeg output options...]
# H.264 and AAC, BT.709 limited range, faststart.
clip() {
  local file="$1" size="$2" rate="$3" seconds="$4"
  shift 4
  # shellcheck disable=SC2046
  "${FF[@]}" $(picture "$size" "$rate") "${TONE[@]}" -t "$seconds" \
    "${AUDIO[@]}" "${H264[@]}" "${BT709[@]}" -shortest -movflags +faststart "$@" \
    "$OUT/$file"
}

clip h264-1280x720.mp4 1280x720 30 20
clip h264-1920x1080.mp4 1920x1080 30 20
# The smoothness check (spec 5.2, criterion 1): 1080p60 for 60 seconds.
clip h264-1920x1080-60fps.mp4 1920x1080 60 60
# Portrait, stored upright.
clip h264-720x1280-portrait.mp4 720x1280 30 10
# QuickTime container, as screen recorders and older iPhones write.
clip h264-1280x720.mov 1280x720 30 10

# Portrait from a phone: stored landscape with a rotation tag of 90 degrees
# clockwise (ffmpeg counts counter-clockwise, hence -90).
clip rotate-source.mp4 1280x720 30 10
"${FF[@]}" -display_rotation:v:0 -90 -i "$OUT/rotate-source.mp4" \
  -c copy -movflags +faststart "$OUT/h264-1280x720-rotate90.mp4"
rm "$OUT/rotate-source.mp4"

# HEVC, tagged hvc1 as Apple devices write it and AVFoundation requires.
ENCODERS="$(ffmpeg -hide_banner -encoders 2>/dev/null)"
if grep -q libx265 <<<"$ENCODERS"; then
  HEVC=(-c:v libx265 -x265-params log-level=error -pix_fmt yuv420p)
else
  HEVC=(-c:v hevc_videotoolbox -q:v 60 -pix_fmt yuv420p)
fi
# shellcheck disable=SC2046
"${FF[@]}" $(picture 1920x1080 30) "${TONE[@]}" -t 10 "${AUDIO[@]}" "${HEVC[@]}" \
  -tag:v hvc1 "${BT709[@]}" -shortest -movflags +faststart "$OUT/hevc-1920x1080.mp4"

# Variable frame rate, as phones record: two of every three frames are
# dropped in the second half of each second, and timestamps are kept.
# shellcheck disable=SC2046
"${FF[@]}" $(picture 1280x720 30) "${TONE[@]}" -t 10 \
  -vf "select='lt(mod(n\,30)\,15)+not(mod(n\,3))'" -fps_mode vfr \
  "${AUDIO[@]}" "${H264[@]}" "${BT709[@]}" -shortest -movflags +faststart \
  "$OUT/h264-1280x720-vfr.mp4"

# No audio track.
# shellcheck disable=SC2046
"${FF[@]}" $(picture 1280x720 30) -t 10 "${H264[@]}" "${BT709[@]}" \
  -movflags +faststart "$OUT/h264-1280x720-noaudio.mp4"

# Audio only.
"${FF[@]}" "${TONE[@]}" -t 10 "${AUDIO[@]}" -movflags +faststart "$OUT/aac-audio-only.m4a"

# The index (moov) at the end of the file: nothing can play until the
# whole file, or its tail, has been read.
# shellcheck disable=SC2046
"${FF[@]}" $(picture 1280x720 30) "${TONE[@]}" -t 10 \
  "${AUDIO[@]}" "${H264[@]}" "${BT709[@]}" -shortest "$OUT/h264-1280x720-moov-at-end.mp4"

# Failures. A file cut off in the middle of its media data (faststart, so
# the index is intact and playback starts):
head -c 400000 "$OUT/h264-1280x720.mp4" > "$OUT/broken-truncated.mp4"
# Bytes that are not media at all:
head -c 65536 /dev/urandom > "$OUT/broken-not-media.mp4"
# A codec no platform decoder has (FFV1), in a container they all open:
# shellcheck disable=SC2046
"${FF[@]}" $(picture 640x360 30) -t 2 -c:v ffv1 "$OUT/unsupported-ffv1.mov"

ls -l "$OUT"
