# gpui-av: spec

Status: experiment. Nothing here is a promise yet. This file is the plan of
record; change it in the same pull request as the work that changes the plan.

`gpui-av` is a community crate from Karya Labs, not part of Zed's GPUI.
The name says what it is: audio and video for GPUI. (Zed's own `gpui-media`
crate is unrelated macOS bindings.)

## 1. Why this exists

GPUI has no video player. Matty (a Mattermost client on GPUI) needs one for
video attachments, and every other GPUI app that shows media hits the same
wall. The crates that exist today are thin and each is locked to one old GPUI
snapshot (section 3.3).

The goal is a media layer for GPUI that feels like it belongs there: one typed
Rust API, the platform's own decoders underneath, and frames that reach the
GPU with as little copying as each platform allows.

## 2. Goals and non-goals

Goals, in order:

1. Play a video attachment in Matty on macOS, Linux and the web: play, pause,
   seek, volume, mute, loop, with sound and picture in sync.
2. A small public API that does not leak platform types and has no `unsafe`
   in it.
3. Measure everything. Every claim about smoothness, CPU, memory or battery
   comes with a number from `experiments/`.
4. Stay usable across GPUI snapshot bumps (section 6, risk R1).

Non-goals for the first six months: Windows, camera, microphone or screen
capture, waveforms, scrubbing previews, a mini player, streaming protocols
(HLS, DASH), DRM, encoding, editing, and bundling any codec.

## 3. Background (verified 2026-11-10/11)

### 3.1 What GPUI gives us

Read from the `gpui-pre` 0.3.12 sources:

- `gpui::surface(CVPixelBuffer)` and `Window::paint_surface` exist on macOS
  and iOS only. Metal draws an NV12 buffer straight from the decoder with no
  copy. Zed uses it for LiveKit screen share.
- The wgpu renderer (Linux, web) skips `PrimitiveBatch::Surfaces` with the
  comment "not implemented". Windows has its own Direct3D 11 renderer whose
  `draw_surfaces` is an empty stub.
- What works everywhere: paint each frame as an `Arc<RenderImage>` and call
  `drop_image` on the previous one. That costs a YUV to BGRA conversion on
  the CPU plus a full texture upload per frame. Zed shows screen share this
  way off macOS.
- `gpui-pre-web` draws into one canvas, preferring WebGPU and falling back to
  WebGL2.

### 3.2 GPU import paths

- wgpu 30 (used by `gpui-pre-wgpu` 0.3.12) has
  `vulkan::Device::texture_from_dmabuf_fd` (gfx-rs/wgpu#9366): a way to show
  VA-API frames on Linux with no copy.
- wgpu's `Device::import_external_texture` for an `HTMLVideoElement` or
  `VideoFrame` (gfx-rs/wgpu#9936) is unreleased as of 2026-11-10, and is
  WebGPU only.
- A published crate cannot carry `[patch.crates-io]` to its users, so any
  renderer change either lands upstream (Zed, then the gpui-pre snapshot) or
  every app patches GPUI itself.
- Zed discussions #45866 (YUV surfaces on Linux/Windows) and #64849 (Windows
  external surfaces) have no maintainer reply.

### 3.3 Prior art

| Crate | Approach | Notes |
|---|---|---|
| gpui-video-player 0.1.3 (MIT) | GStreamer on every OS, surface on macOS, RenderImage elsewhere | Pins gpui-pre =0.3.9; Mac users must install GStreamer |
| alhazen-gpui / alhazen-core (Apache-2.0) | Pure-Rust decoders plus VideoToolbox | Targets gpui 0.2; forks pin old gpui-pre |
| ely-media (MIT) | Media components | Pins gpui-pre =0.3.10 |
| gpui_interop 0.2.1 (Apache-2.0) | dma-buf (NV12) on Linux, D3D shared textures on Windows | For the bite-gpui fork only |

Read these before writing the same thing again; contributing upstream to one
of them is a valid outcome of the experiments.

## 4. Architecture

```
            VideoPlayer (GPUI entity: state, events, commands)
                 |                                 |
        Backend (trait, per platform)       Presenter (how frames reach GPUI)
   AVFoundation | GStreamer | Web video     surface | RenderImage | DOM overlay
```

- **Backend.** Opens a source (file path or URL with auth headers), owns
  decode, audio output and the clock, and reports state. Backends:
  - macOS: `AVPlayer` + `AVPlayerItemVideoOutput`, giving `CVPixelBuffer`s.
  - Linux: GStreamer `playbin` with an `appsink`, loaded at run time so the
    app still starts without GStreamer; without it, fall back to opening the
    file in the system player.
  - Web: an `HTMLVideoElement`. Later, WebCodecs `VideoDecoder` plus a Rust
    MP4 demuxer for frames inside the canvas.
- **Presenter.** Picks the cheapest path the platform has: `surface` on
  macOS; `RenderImage` on Linux; a DOM `<video>` placed over the element's
  bounds on the web (first cut; menus cannot draw over it). Zero-copy
  presenters for wgpu are a research track (section 5, E5), not a dependency.
- **Clock.** Audio is the master clock wherever the backend gives us one;
  video frames are shown when their timestamp is due and dropped when late.

### 4.1 API sketch (draft)

```rust
let player = cx.new(|cx| VideoPlayer::new(Source::url(url).header("Authorization", token), cx));
player.update(cx, |p, cx| p.play(cx));

// In render:
video(&player).object_fit(ObjectFit::Contain).size_full()

// State and events
enum PlaybackState { Loading, Ready, Playing, Paused, Buffering, Ended, Failed(Error) }
cx.subscribe(&player, |this, player, event: &PlayerEvent, cx| { /* ... */ });
```

Commands: `play`, `pause`, `seek(Duration)`, `set_volume(f32)`, `set_muted`,
`set_looping`. Queries: `position`, `duration`, `buffered`, `natural_size`,
`state`. Controls (play button, timeline, volume, fullscreen) live in gpuicn,
not here, so gpui-av stays a media crate.

## 5. Experiments

Each experiment is a folder under `experiments/` with a README holding the
question, the setup, the result and the numbers. An experiment answers one
question; when it is done, the answer is folded into this spec.

| ID | Question | Done when |
|---|---|---|
| E0 | What do real Mattermost videos look like (containers, codecs, sizes, lengths)? | A table from a sample of real attachments |
| E1 | macOS: does AVPlayer + `surface()` play 1080p60 smoothly inside a GPUI window? | A demo plays with sound; CPU and dropped frames measured |
| E2 | Linux: GStreamer + RenderImage at 1080p: what does the per-frame copy cost? | CPU, memory and dropped frames at 720p and 1080p |
| E3 | Web: a DOM `<video>` overlay that tracks a GPUI element through scroll, resize and close | Works in Chrome, Firefox and Safari |
| E4 | Web: WebCodecs frames drawn inside the canvas without an overlay | One clip plays in Chrome; cost measured against E3 |
| E5 | Zero-copy on wgpu: implement `PrimitiveBatch::Surfaces` (NV12) in gpui-pre-wgpu | Patch works locally; decide whether to propose it to Zed |
| E6 | Can we build on gpui-video-player or alhazen instead of writing backends? | A written yes or no with reasons |

Order: E0 and E6 first (cheap, and they can cancel work), then E1 and E3
(the two biggest user groups after the web), then E2, E4 and E5.

Test clips are generated with a script (for example ffmpeg's `testsrc2` and
`sine` sources) rather than committed; the repo ignores video files.

## 6. Risks

- **R1. GPUI churn.** gpui-pre ships a new snapshot about weekly, and the
  existing video crates broke on exactly this. Keep the surface we touch
  small, and run a canary against new snapshots.
- **R2. Renderer ownership.** Zero-copy outside macOS needs renderer changes
  we do not own. Treat it as optional; nothing in goals 1 to 4 depends on it.
- **R3. Small audience.** About 100 to 150 crates depend on GPUI. Matty
  alone justifies the work; wider adoption is a bonus.
- **R4. Codecs and law.** Use only the decoders the OS or browser already
  has; never bundle GStreamer plugins or ffmpeg. Not legal advice.
- **R5. Scope.** One maintainer plus Claude. Keep to the non-goals.

## 7. Decisions

- 2026-11-12: named `gpui-av` (repo `karyalabs/gpui-av`).
- 2026-11-10: native decoders, not ffmpeg (size, licensing, patents, own A/V
  sync). ffmpeg.wasm also rejected for the web (software decode, about
  25 to 30 MB, slow past 720p, needs cross-origin isolation for threads).
- 2026-11-10: a Fable 5.2 review passed the idea at about 75% confidence, on
  condition of the slim scope above.

## 8. Reading

1. Leandro Moreira, *Digital video introduction*:
   github.com/leandromoreira/digital_video_introduction
2. Sid Bala, *H.264 is Magic*: sidbala.com/h-264-is-magic
3. Leandro Moreira, *Learn FFmpeg libav the hard way*:
   github.com/leandromoreira/ffmpeg-libav-tutorial
4. Dranger, *An ffmpeg and SDL tutorial* (A/V sync): dranger.com/ffmpeg
5. MDN: *Web video codec guide* and the WebCodecs API; Chrome's *Video
   processing with WebCodecs*
6. GStreamer basic tutorials; Apple's `AVPlayerItemVideoOutput` docs
7. Demuxed conference talks
