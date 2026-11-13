# gpui-av

Video playback for [GPUI](https://www.gpui.rs) apps, using each platform's own
decoders: AVFoundation on macOS, GStreamer on Linux, and the browser on the web.

A Karya Labs community project; not part of or endorsed by Zed.

**Status: experiment.** A proof of concept plays video on macOS and in
Chrome through one API: the API crate, the macOS and web backends and the
lab exist; Linux is next, and nothing is published. The plan, the open
questions and the experiments that answer them are in
[docs/spec.md](docs/spec.md).

## Try it

The lab plays any folder of videos, natively or in a browser:

```sh
cargo run --release -p gpui-av-lab -- ~/Movies   # macOS

scripts/web setup            # once: the wasm toolchain pieces
scripts/web serve ~/Movies   # then open http://127.0.0.1:8090/
```

`scripts/fetch-real-clips.sh` fetches Blender's open films for judging
playback by eye and ear; `scripts/make-clips.sh` makes the synthetic clips
the bench uses. No video is committed here.

Part of [Karya Labs](https://github.com/karyalabs), alongside
[Matty](https://github.com/karyalabs/matty) and
[gpuicn](https://github.com/karyalabs/gpuicn).

## Layout

- `docs/spec.md`: the plan of record.
- `crates/gpui-av`: the API: the `VideoPlayer` entity, the `video()` element
  and the public `Backend` trait. No `unsafe`, no platform types.
- `crates/gpui-av-avfoundation`: the macOS backend (AVFoundation).
- `crates/gpui-av-web`: the web backend (a `<video>` element kept over the
  GPUI element, on top of the canvas).
- `lab/`: `gpui-av-lab`, the playground. `cargo run -p gpui-av-lab -- clips/`
  plays a folder; with no arguments it plays built-in test patterns.
- `scripts/`: the web build and its server (`web`, `serve.py`), the clip
  scripts, and a dev-only virtual screen for measuring on a Mac in use.
- `experiments/`: one folder per experiment, each with a README holding the
  question, setup, result and numbers.

## License

MIT
