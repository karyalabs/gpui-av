#!/usr/bin/env bash
# Fetches real films with real sound, for judging playback by eye and ear:
# smoothness, lip sync, colour. The synthetic corpus (make-clips.sh) is for
# the bench; these are for people. Nothing is committed and nothing is
# bundled: they come from Blender's own servers, under Creative Commons.
#
# Usage: scripts/fetch-real-clips.sh [dir] [--all]
#   default  Sintel trailer, 1080p24 H.264 + AAC (15 MB)
#   --all    also Big Buck Bunny 1080p60 (355 MB zip, 10 min, the
#            smoothness check by eye) and Tears of Steel 720p (372 MB,
#            live action with dialogue: watch the lips for A/V sync)
# Then:    cargo run --release -p gpui-av-lab -- <dir>
#
# Downloads resume if interrupted. Each file's SHA-256 is printed and kept
# next to it (<file>.sha256); a later fetch checks against it.
set -euo pipefail
OUT="clips/real"
ALL=false
for arg in "$@"; do
  case "$arg" in
    --all) ALL=true ;;
    -h | --help) sed -n '2,17p' "$0"; exit 0 ;;
    *) OUT="$arg" ;;
  esac
done
mkdir -p "$OUT"
BASE=https://download.blender.org

# fetch <url> <file in OUT> [member to extract from a zip]
fetch() {
  local url="$1" file="$2" member="${3:-}"
  local download="$OUT/${url##*/}"
  if [[ -f "$OUT/$file" ]]; then
    echo "have $file"
  else
    echo "fetching $url"
    curl -fL --retry 3 -C - -o "$download.part" "$url"
    mv "$download.part" "$download"
    if [[ -n "$member" ]]; then
      unzip -o -j -q "$download" "$member" -d "$OUT"
      rm "$download"
    fi
  fi
  local sum
  sum="$(shasum -a 256 "$OUT/$file" | cut -d' ' -f1)"
  if [[ -f "$OUT/$file.sha256" ]] && [[ "$(cat "$OUT/$file.sha256")" != "$sum" ]]; then
    echo "SHA-256 of $file changed: $sum, was $(cat "$OUT/$file.sha256")" >&2
    exit 1
  fi
  echo "$sum" > "$OUT/$file.sha256"
  echo "  $file  sha256 $sum"
}

# Sintel trailer (Blender Foundation, CC BY 3.0).
fetch "$BASE/durian/trailer/sintel_trailer-1080p.mp4" sintel_trailer-1080p.mp4
if $ALL; then
  # Big Buck Bunny, 60 fps remaster (Blender Foundation, CC BY 3.0).
  fetch "$BASE/demo/movies/BBB/bbb_sunflower_1080p_60fps_normal.mp4.zip" \
    bbb_sunflower_1080p_60fps_normal.mp4 "*bbb_sunflower_1080p_60fps_normal.mp4"
  # Tears of Steel (Blender Foundation, CC BY 3.0).
  fetch "$BASE/demo/movies/ToS/tears_of_steel_720p.mov" tears_of_steel_720p.mov
fi
ls -l "$OUT"
