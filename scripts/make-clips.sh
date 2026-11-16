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
# Colour: BT.709, limited range, stated in the stream. Some ffmpeg builds
# (seen on a Mac) do not pass -color_primaries and -color_trc on to
# libx264 and libx265, which left the clips without a transfer function,
# so the encoders are told directly as well.
X26_BT709="colorprim=bt709:transfer=bt709:colormatrix=bt709"
H264=(-c:v libx264 -x264-params "$X26_BT709" -pix_fmt yuv420p)
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
  HEVC=(-c:v libx265 -x265-params "log-level=error:$X26_BT709" -pix_fmt yuv420p)
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

# Tracks to choose between: two audio tracks (the second a little higher,
# so a switch can be heard) and English subtitles as mov_text.
printf '1\n00:00:01,000 --> 00:00:04,000\nThe first line\n\n2\n00:00:05,000 --> 00:00:08,000\nThe second line\n' \
  > "$OUT/tracks.srt"
# shellcheck disable=SC2046
"${FF[@]}" $(picture 1280x720 30) "${TONE[@]}" -f lavfi -i "sine=frequency=660" \
  -i "$OUT/tracks.srt" -t 10 -map 0:v -map 1:a -map 2:a -map 3:s \
  "${AUDIO[@]}" "${H264[@]}" "${BT709[@]}" -c:s mov_text \
  -metadata:s:a:0 language=eng -metadata:s:a:1 language=fra \
  -metadata:s:a:1 title=Commentary -metadata:s:s:0 language=eng \
  -movflags +faststart "$OUT/h264-1280x720-tracks.mp4"
# The same subtitles as SubRip in Matroska, which Windows' Matroska source
# hands on as text where its MP4 source drops 3GPP text.
# shellcheck disable=SC2046
"${FF[@]}" $(picture 1280x720 30) "${TONE[@]}" -i "$OUT/tracks.srt" -t 10 \
  -map 0:v -map 1:a -map 2:s "${AUDIO[@]}" "${H264[@]}" "${BT709[@]}" -c:s srt \
  -metadata:s:s:0 language=eng "$OUT/h264-1280x720-subtitles.mkv"
rm "$OUT/tracks.srt"

# Audio only.
"${FF[@]}" "${TONE[@]}" -t 10 "${AUDIO[@]}" -movflags +faststart "$OUT/aac-audio-only.m4a"

# The index (moov) at the end of the file: nothing can play until the
# whole file, or its tail, has been read.
# shellcheck disable=SC2046
"${FF[@]}" $(picture 1280x720 30) "${TONE[@]}" -t 10 \
  "${AUDIO[@]}" "${H264[@]}" "${BT709[@]}" -shortest "$OUT/h264-1280x720-moov-at-end.mp4"

# HLS of the 20 s clip, cut without re-encoding: MPEG-TS segments (what
# most servers send) and fragmented MP4 ones, each in its own folder.
mkdir -p "$OUT/hls-ts" "$OUT/hls-fmp4"
"${FF[@]}" -i "$OUT/h264-1280x720.mp4" -c copy -f hls -hls_time 4 \
  -hls_playlist_type vod -hls_segment_filename "$OUT/hls-ts/%d.ts" "$OUT/hls-ts/index.m3u8"
"${FF[@]}" -i "$OUT/h264-1280x720.mp4" -c copy -f hls -hls_time 4 \
  -hls_playlist_type vod -hls_segment_type fmp4 -hls_fmp4_init_filename init.mp4 \
  -hls_segment_filename "$OUT/hls-fmp4/%d.m4s" "$OUT/hls-fmp4/index.m3u8"

# Failures. A file cut off in the middle of its media data (faststart, so
# the index is intact and playback starts):
head -c 400000 "$OUT/h264-1280x720.mp4" > "$OUT/broken-truncated.mp4"
# Bytes that are not media at all:
head -c 65536 /dev/urandom > "$OUT/broken-not-media.mp4"
# A codec no platform decoder has (FFV1), in a container they all open:
# shellcheck disable=SC2046
"${FF[@]}" $(picture 640x360 30) -t 2 -c:v ffv1 "$OUT/unsupported-ffv1.mov"

ls -l "$OUT"
