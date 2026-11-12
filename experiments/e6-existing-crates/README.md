# E6: build on an existing crate?

## Question

Can gpui-av build on `gpui-video-player` or `alhazen` instead of writing its
own backends?

## Setup

- 2026-11-12, cloud Linux container (4 cores, no GPU), Rust 1.97.
- `gpui-video-player` 0.1.3 and `alhazen-core` 0.7.1 from crates.io, read in
  full or by features. GStreamer 1.24.2 from Ubuntu 24.04.
- Compile check: `gpui-video-player` with its `=0.3.9` pins changed to
  `=0.3.12` (Matty's gpui-pre), then
  `cargo check --no-default-features --features gpui-pre --examples`.
- Not done: running the player. The sandbox blocked building and running
  third-party example code, so there are no playback numbers for it here.

## Result

**No to both as dependencies. Yes to `gpui-video-player` as reference code**
(MIT, so we can reuse parts with attribution).

### gpui-video-player 0.1.3

It compiles against gpui-pre 0.3.12 with only the version pin changed (58 s,
no errors), so the API GPUI exposes for this is stable across snapshots. That
is good news for risk R1.

What it does: GStreamer `playbin` on every OS, an `appsink` forced to NV12,
a worker thread polling for samples every 16 ms, and a GPUI element that
paints the newest frame. On macOS it copies each frame into a new
`CVPixelBuffer` and uses `paint_surface`. Elsewhere it converts NV12 to BGRA
on the CPU (`yuv` crate) and paints a `RenderImage`, dropping the previous one.

Why we should not depend on it:

1. **GStreamer on macOS.** Mac users would have to install GStreamer. We want
   AVFoundation there.
2. **No request headers.** The source is a URL handed to `playbin`, with no way
   to add `Authorization`. Mattermost file URLs need it, so Matty would have to
   download the whole file first or build its own pipeline.
3. **Wrong colours for most videos.** The CPU conversion tries full range
   first and only falls back to limited range if that call fails, which it
   never does. The macOS path tags every frame as full range. Phone and
   screen recordings are almost always limited range (BT.709), so blacks and
   whites come out washed out. The caps' colorimetry is never read.
4. **Software conversion is forced.** `videoscale ! videoconvert !
   video/x-raw,format=NV12` in the sink means hardware-decoded frames are
   copied back to the CPU, so there is no path to no-copy later.
5. **It blocks the UI thread.** `Video::new` waits up to 5 s for the pipeline
   to start.
6. **No web backend.**

Worth borrowing: the command set (pause, accurate and keyframe seek, speed,
loop, volume, mute), dropping the previous `RenderImage` with
`drop_image`, and only requesting animation frames while playing.

Fixes 2, 3 and 5 would make good upstream pull requests if we want goodwill in
the GPUI community, but 1, 4 and 6 are design choices, not bugs.

### alhazen-core 0.7.1

A Rust media engine with its own decoders (AV1 via rav1d, VP9, ProRes, AAC,
MP3, Opus) plus VideoToolbox, Media Foundation and NVDEC. Its GPUI crate
targets GPUI 0.2, not gpui-pre.

Why not:

- **No general H.264 path on Linux.** H.264 comes from VideoToolbox (macOS),
  Media Foundation (Windows), NVDEC (NVIDIA only) or an `ffmpeg-cli` feature.
  Most Linux users would get nothing for the most common format.
- **It bundles codecs** (AAC, MP3), which our spec rules out (risk R4).
- **It owns audio output and A/V sync** through `cpal`, a large surface for us
  to depend on when the platform players already do this.

Worth borrowing: its VideoToolbox code, when we write the macOS backend.

## Side measurement: GStreamer software decode cost

Decoding only, no display, `avdec_h264 ! videoconvert ! NV12 ! fakesink
sync=true`, 20 s synthetic clips from `scripts/make-clips.sh`:

| Clip | CPU over 20 s | Share of one core |
|---|---|---|
| 720p30 H.264 | 2.8 s | ~14% |
| 1080p30 H.264 | 5.0 s | ~25% |

NV12 to BGRA with ffmpeg's swscale: 300 frames at 1080p in 1.46 s of CPU,
about 4.9 ms a frame, or roughly 15% of a core at 30 fps.

So the Linux `RenderImage` path costs about 0.4 core at 1080p30 before
GPUI's own redraw. That is fine for chat clips. Synthetic test patterns
compress better than real footage, so treat these as lower bounds. E2 measures
the full path in a window.

## Next

- Spec: E6 answered. Write our own backends behind a small trait, borrowing
  from `gpui-video-player` (MIT) where it helps.
- The backend trait must take request headers from day one (Mattermost auth).
- Read colorimetry (range and matrix) from the decoder and pass it to the
  presenter; never guess.
- Optional: upstream fixes 3 and 5 to `gpui-video-player`.
