# gpui-av

Video playback for [GPUI](https://www.gpui.rs) apps, using each platform's own
decoders: AVFoundation on macOS, GStreamer on Linux, and the browser on the web.

A Karya Labs community project; not part of or endorsed by Zed.

**Status: experiment.** There is no crate yet. The plan, the open questions and
the experiments that answer them are in [docs/spec.md](docs/spec.md).

Part of [Karya Labs](https://github.com/karyalabs), alongside
[Matty](https://github.com/karyalabs/matty) and
[gpuicn](https://github.com/karyalabs/gpuicn).

## Layout

- `docs/spec.md`: the plan of record.
- `experiments/`: one folder per experiment, each with a README holding the
  question, setup, result and numbers.

## License

MIT
