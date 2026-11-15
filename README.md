# gpui-av

Video playback for [GPUI](https://www.gpui.rs) apps, using each platform's own
decoders: AVFoundation on macOS, Media Foundation on Windows, GStreamer on
Linux, and the browser on the web.

A Karya Labs community project; not part of or endorsed by Zed.

**Status: early, shipping continuously.** Video plays on macOS, Linux and
in Chrome through one API; nothing is published to crates.io yet. The plan,
the roadmap (section 5.3) and the experiments behind it are in
[docs/spec.md](docs/spec.md).

## Use it

```rust
// Once, at startup: the right backends for this platform, tried in order.
gpui_av_platform::init(cx);

// A player, from a file or a URL with the headers it needs.
let source = Source::url(url).header("Authorization", token);
let player = cx.new(|cx| VideoPlayer::new(source, cx));
player.update(cx, |player, _| player.play());

// In render:
video(&player).object_fit(ObjectFit::Contain).size_full()
```

`gpui-av-platform` uses AVFoundation on macOS and falls back to GStreamer
(where installed) for media AVFoundation will not open, such as MKV;
Media Foundation on Windows, then GStreamer; GStreamer on Linux; and the browser on the web. Each backend is
a cargo feature. To choose your own order, or add a backend of your own,
build a `gpui_av::Fallback` and pass it to `gpui_av::set_default_backend`.

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
- `crates/gpui-av-gstreamer`: the Linux backend (GStreamer, loaded at run
  time). It also builds for macOS and Windows, where it is used if
  GStreamer is installed; it has not been played there yet.
- `crates/gpui-av-mediafoundation`: the Windows backend (Media
  Foundation's media engine).
- `crates/gpui-av-platform`: picks the platform's backends in one call.
- `lab/`: `gpui-av-lab`, the playground. `cargo run -p gpui-av-lab -- clips/`
  plays a folder; with no arguments it plays built-in test patterns.
- `scripts/`: the web build and its server (`web`, `serve.py`), the clip
  scripts, and a dev-only virtual screen for measuring on a Mac in use.
- `experiments/`: one folder per experiment, each with a README holding the
  question, setup, result and numbers.

## License

MIT
