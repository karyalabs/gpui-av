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
