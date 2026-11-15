#!/usr/bin/env bash
# Generates the synthetic tier B corpus (spec 5.1): coverage beyond phone
# clips. Every clip should play on every backend; one that fails is a
# coverage gap to close, and the bench's coverage table lists them. The
# clips and what each must report are in corpus/tier-b.json; keep the two
# in step. Needs ffmpeg with libx264, libx265, libvpx (VP9), an AV1 encoder
# (libsvtav1 or libaom) and libopus. Video files are never committed.
# Usage: scripts/make-tier-b.sh [dir]
set -euo pipefail
OUT="${1:-clips-b}"
# SVT-AV1 prints its configuration unless told to log errors only.
export SVT_LOG=1
mkdir -p "$OUT"

FF=(ffmpeg -loglevel error -y)
# A quiet tone, as in tier A.
TONE=(-f lavfi -i "sine=frequency=440")
QUIET=(-af volume=0.05)
AAC=("${QUIET[@]}" -c:a aac)
OPUS=("${QUIET[@]}" -c:a libopus -b:a 96k)
X26_BT709="colorprim=bt709:transfer=bt709:colormatrix=bt709"
BT709=(-color_range tv -colorspace bt709 -color_primaries bt709 -color_trc bt709)
H264=(-c:v libx264 -preset veryfast -x264-params "$X26_BT709" -pix_fmt yuv420p)
HEVC=(-c:v libx265 -preset veryfast -tag:v hvc1)
VP9=(-c:v libvpx-vp9 -deadline realtime -cpu-used 8 -row-mt 1 -b:v 2M -pix_fmt yuv420p)
ENCODERS="$(ffmpeg -hide_banner -encoders 2>/dev/null)"
if grep -q libsvtav1 <<<"$ENCODERS"; then
  AV1=(-c:v libsvtav1 -preset 10 -crf 40 -pix_fmt yuv420p)
else
  AV1=(-c:v libaom-av1 -cpu-used 8 -row-mt 1 -crf 40 -b:v 0 -pix_fmt yuv420p)
fi

picture() { # <size> <rate>
  echo -f lavfi -i "testsrc2=size=$1:rate=$2"
}

# Codecs and containers beyond H.264 in MP4. Five seconds each.
# shellcheck disable=SC2046
"${FF[@]}" $(picture 1280x720 30) "${TONE[@]}" -t 5 "${VP9[@]}" "${BT709[@]}" \
  "${OPUS[@]}" "$OUT/vp9-opus-1280x720.webm"
# shellcheck disable=SC2046
"${FF[@]}" $(picture 1280x720 30) "${TONE[@]}" -t 5 "${VP9[@]}" "${BT709[@]}" \
  "${AAC[@]}" -movflags +faststart "$OUT/vp9-aac-1280x720.mp4"
# shellcheck disable=SC2046
"${FF[@]}" $(picture 1280x720 30) "${TONE[@]}" -t 5 "${AV1[@]}" "${BT709[@]}" \
  "${OPUS[@]}" "$OUT/av1-opus-1280x720.webm"
# shellcheck disable=SC2046
"${FF[@]}" $(picture 1280x720 30) "${TONE[@]}" -t 5 "${AV1[@]}" "${BT709[@]}" \
  "${AAC[@]}" -movflags +faststart "$OUT/av1-aac-1280x720.mp4"
# Matroska, as downloads and rips come.
# shellcheck disable=SC2046
"${FF[@]}" $(picture 1280x720 30) "${TONE[@]}" -t 5 "${H264[@]}" "${BT709[@]}" \
  "${AAC[@]}" "$OUT/h264-aac-1280x720.mkv"
# shellcheck disable=SC2046
"${FF[@]}" $(picture 1280x720 30) "${TONE[@]}" -t 5 "${HEVC[@]}" \
  -x265-params "log-level=error:$X26_BT709" -pix_fmt yuv420p "${BT709[@]}" \
  "${OPUS[@]}" "$OUT/hevc-opus-1280x720.mkv"
# Fragmented MP4, as live streams and some cameras write.
# shellcheck disable=SC2046
"${FF[@]}" $(picture 1280x720 30) "${TONE[@]}" -t 5 "${H264[@]}" "${BT709[@]}" \
  "${AAC[@]}" -movflags +frag_keyframe+empty_moov+default_base_moof \
  "$OUT/h264-1280x720-fragmented.mp4"

# Size and rate.
# shellcheck disable=SC2046
"${FF[@]}" $(picture 3840x2160 60) "${TONE[@]}" -t 5 "${H264[@]}" "${BT709[@]}" \
  "${AAC[@]}" -movflags +faststart "$OUT/h264-3840x2160-60fps.mp4"
# A size that is not a multiple of 16, as cropped exports are.
# shellcheck disable=SC2046
"${FF[@]}" $(picture 854x480 30) "${TONE[@]}" -t 5 "${H264[@]}" "${BT709[@]}" \
  "${AAC[@]}" -movflags +faststart "$OUT/h264-854x480.mp4"
# Non-square pixels: 720x576 stored, 16:15 pixels, shown 768x576.
# shellcheck disable=SC2046
"${FF[@]}" $(picture 720x576 25) "${TONE[@]}" -t 5 -vf setsar=16/15 "${H264[@]}" \
  "${BT709[@]}" "${AAC[@]}" -movflags +faststart "$OUT/h264-720x576-anamorphic.mp4"

# Colour.
# shellcheck disable=SC2046
"${FF[@]}" $(picture 1280x720 30) "${TONE[@]}" -t 5 -c:v libx264 -preset veryfast \
  -x264-params "$X26_BT709:fullrange=on" -pix_fmt yuvj420p -color_range pc \
  -colorspace bt709 -color_primaries bt709 -color_trc bt709 \
  "${AAC[@]}" -movflags +faststart "$OUT/h264-1280x720-fullrange.mp4"
# shellcheck disable=SC2046
"${FF[@]}" $(picture 640x480 30) "${TONE[@]}" -t 5 -c:v libx264 -preset veryfast \
  -x264-params "colorprim=smpte170m:transfer=smpte170m:colormatrix=smpte170m" \
  -pix_fmt yuv420p -color_range tv -colorspace smpte170m \
  -color_primaries smpte170m -color_trc smpte170m \
  "${AAC[@]}" -movflags +faststart "$OUT/h264-640x480-bt601.mp4"
# shellcheck disable=SC2046
"${FF[@]}" $(picture 1920x1080 30) "${TONE[@]}" -t 5 "${HEVC[@]}" \
  -x265-params "log-level=error:$X26_BT709" -pix_fmt yuv420p10le "${BT709[@]}" \
  "${AAC[@]}" -movflags +faststart "$OUT/hevc-10bit-1920x1080.mp4"
HDR10="colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:hdr10=1"
HDR10="$HDR10:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll=1000,400"
# shellcheck disable=SC2046
"${FF[@]}" $(picture 1920x1080 30) "${TONE[@]}" -t 5 "${HEVC[@]}" \
  -x265-params "log-level=error:$HDR10" -pix_fmt yuv420p10le -color_range tv \
  -colorspace bt2020nc -color_primaries bt2020 -color_trc smpte2084 \
  "${AAC[@]}" -movflags +faststart "$OUT/hevc-hdr10-1920x1080.mp4"
# shellcheck disable=SC2046
"${FF[@]}" $(picture 1920x1080 30) "${TONE[@]}" -t 5 "${HEVC[@]}" \
  -x265-params "log-level=error:colorprim=bt2020:transfer=arib-std-b67:colormatrix=bt2020nc" \
  -pix_fmt yuv420p10le -color_range tv -colorspace bt2020nc -color_primaries bt2020 \
  -color_trc arib-std-b67 "${AAC[@]}" -movflags +faststart "$OUT/hevc-hlg-1920x1080.mp4"

# Sound.
# shellcheck disable=SC2046
"${FF[@]}" $(picture 1280x720 30) -f lavfi -i "sine=frequency=440" -t 5 "${H264[@]}" \
  "${BT709[@]}" -af "volume=0.05,pan=5.1|FL=c0|FR=c0|FC=c0|LFE=c0|BL=c0|BR=c0" \
  -c:a aac -movflags +faststart "$OUT/h264-aac51-1280x720.mp4"
"${FF[@]}" "${TONE[@]}" -t 5 "${OPUS[@]}" "$OUT/opus-audio-only.webm"

# A long file, for seeking far: ten minutes, small.
# shellcheck disable=SC2046
"${FF[@]}" $(picture 320x180 30) "${TONE[@]}" -t 600 -c:v libx264 -preset veryfast -g 300 \
  -crf 40 -x264-params "$X26_BT709" -pix_fmt yuv420p "${BT709[@]}" \
  "${AAC[@]}" -movflags +faststart "$OUT/h264-320x180-10min.mp4"

# Corrupt: a good file with a run of its media data overwritten, so the
# index is intact and decoding meets garbage in the middle. Players
# should play through it or fail with Decode, never hang.
cp "$OUT/h264-854x480.mp4" "$OUT/broken-corrupt-middle.mp4"
SIZE=$(wc -c < "$OUT/broken-corrupt-middle.mp4")
head -c 200000 /dev/urandom | dd of="$OUT/broken-corrupt-middle.mp4" bs=1 \
  seek=$((SIZE / 2)) conv=notrunc status=none

ls -l "$OUT"
