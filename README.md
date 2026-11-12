# gpui-av

Video playback for [GPUI](https://www.gpui.rs) apps, using each platform's own
decoders: AVFoundation on macOS, GStreamer on Linux, and the browser on the web.

A Karya Labs community project; not part of or endorsed by Zed.

**Status: experiment.** A proof of concept is being built: the API crate
exists, the backends do not yet, and nothing is published. The plan, the
open questions and the experiments that answer them are in
[docs/spec.md](docs/spec.md).

Part of [Karya Labs](https://github.com/karyalabs), alongside
[Matty](https://github.com/karyalabs/matty) and
[gpuicn](https://github.com/karyalabs/gpuicn).

## Layout

- `docs/spec.md`: the plan of record.
- `crates/gpui-av`: the API: the `VideoPlayer` entity, the `video()` element
  and the public `Backend` trait. No `unsafe`, no platform types.
- `crates/gpui-av-avfoundation`, `crates/gpui-av-web`: the macOS and web
  backends (placeholders for now).
- `experiments/`: one folder per experiment, each with a README holding the
  question, setup, result and numbers.

## License

MIT
