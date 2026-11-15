# Corpus

`tier-a.json` lists the synthetic tier A clips (spec section 5.1): what
`scripts/make-clips.sh` generates and what a backend must report for each.
No media is committed; generate it and bench it with:

```sh
scripts/make-clips.sh clips
cargo run --release -p gpui-av-lab -- --bench corpus/tier-a.json --clips clips --out bench.json
```

Each entry has the file name, whether it must play or fail (`expect`, and
for failures the acceptable error kinds in `errors`), and the properties
the bench compares: `width` and `height` as shown (after rotation),
`rotation`, `duration`, `video_codec` and `audio_codec` (absent when the
track is), `color_range` and `color_matrix`. `fps` is absent for variable
frame rate. `play_seconds` overrides how long the bench measures the clip.

When you change the script, change the manifest in the same commit.

`tier-b.json` is the coverage tier: VP9 and AV1 in WebM and MP4, Matroska,
fragmented MP4, 4K60, odd sizes, non-square pixels, full range, BT.601,
10-bit, HDR10 and HLG, 5.1, Opus, a ten minute file and a corrupt one.
Every clip should play everywhere; the bench ends with a coverage table
(clip against backend) so the gaps stay visible:

```sh
scripts/make-tier-b.sh clips-b
cargo run --release -p gpui-av-lab -- --bench corpus/tier-b.json --clips clips-b --out bench-b.json
```

## Real films, for people

The synthetic clips are for the bench: test patterns and a tone are easy
to check by machine and useless for judging playback by eye and ear. For
that, `scripts/fetch-real-clips.sh` fetches real films with real sound
from Blender's servers (Creative Commons): the Sintel trailer (15 MB) by
default, and with `--all` Big Buck Bunny 1080p60 (355 MB, 10 minutes, the
smoothness check by eye) and Tears of Steel 720p (372 MB, live action with
dialogue, for lip sync). Then open the folder in the lab, with sound:

```sh
scripts/fetch-real-clips.sh clips/real --all
cargo run --release -p gpui-av-lab -- clips/real
```

The lab plays any file, folder or URL the same way, so your own videos
work too: `cargo run --release -p gpui-av-lab -- ~/Movies/some-folder`.
In a browser, `scripts/web serve ~/Movies/some-folder` (or `clips/real`)
serves the lab with that folder listed.
