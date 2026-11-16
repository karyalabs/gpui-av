# gpui-av: spec

Status: experiment. Nothing here is a promise yet. This file is the plan of
record; change it in the same pull request as the work that changes the plan.

`gpui-av` is a community crate from Karya Labs, not part of Zed's GPUI.
The name says what it is: audio and video for GPUI. (Zed's own `gpui-media`
crate is unrelated macOS bindings.)

## 0. Start here

**Where things stand (2026-11-15).** The proof of concept (section 5.2) is
done: the workspace, the `gpui-av` API crate, the macOS backend
(`gpui-av-avfoundation`), the web backend (`gpui-av-web`) and the lab
(`lab/`) exist (section 4.2), and the tier A corpus plays through one API
on macOS and in Chrome. E1 is answered: AVPlayer drawn with `surface`
plays 1080p60 with no dropped frames at about a fifth of a core. The web
backend (E3) plays the whole tier A corpus in Chrome, with flat memory
over 10 minutes and an overlay that follows resize, scroll and a move
between screens of different pixel ratio; Safari and Firefox are not run
yet. E6 is done (section 5): we write our own backends and borrow ideas
from `gpui-video-player`. E2 is answered (2026-11-13, real GPU numbers 2026-11-14):
the Linux backend (`gpui-av-gstreamer`) plays the tier A corpus and 4K
10-bit clips with NVDEC, but drawing through `RenderImage` costs about as
much CPU per frame as decoding. E5's first step (NV12 drawn by our GPUI,
karyalabs/zed#7) cuts that CPU by half to three quarters on an NVIDIA A10;
taking frames as dmabufs is the next step for Linux. When gpui-av works, its player
moves into gpuicn (Karya Labs' GPUI component library) as a component, and
Matty uses it from there. Until then this repo stands alone: no other Karya
Labs repo depends on it, and nothing here needs Matty or gpuicn to build.

The first thing built was the proof of concept in section 5.2: two
platforms (macOS and the web), the real crate, the lab, and numbers.
Since 2026-11-15 the work ships continuously as small pull requests along
the roadmap in section 5.3, rather than pausing between experiments.

**Picking up work in a new thread.** Each experiment in section 5 is sized
for one thread. To start one, give the thread this repo and a prompt like:

> Run experiment E3 from `docs/spec.md` in karyalabs/gpui-av. Read
> `AGENTS.md` and the spec first. Work on a branch named after the
> experiment, put code and the write-up in `experiments/e3-<slug>/`, fold
> the answer into the spec, and open a pull request.

**What a finished experiment looks like.**

- A folder `experiments/<id>-<slug>/` with a README: Question, Setup
  (machine, OS, GPUI version, clip, how to run), Result (the answer, with
  numbers), Next (what changes in the spec). `experiments/e6-existing-crates`
  is the example to copy.
- Its row in the section 5 table marked done, with the date and a link.
- Any decision it settles added to section 7.

**Where each experiment can run.**

| Experiment | Needs |
|---|---|
| E0 | Access to a real Mattermost server with video attachments (hint: the files search API, `POST /api/v4/teams/{team_id}/files/search`, with terms like `ext:mp4`) |
| E1 | A Mac (AVFoundation, Metal) |
| E2 | Linux; a desktop with a GPU gives real numbers, a cloud container only gives CPU numbers. An OVH GPU instance works (`experiments/e2-gstreamer/ovh`) |
| E3, E4 | A browser; Chrome works in a cloud container, Firefox and Safari need a real machine |
| E5 | Linux with a Vulkan GPU (an OVH A10 for NVIDIA); it patches gpui-pre-wgpu locally |
| E6 | Done |
| E7 | A Mac and Chrome; MKV clips from the corpus |
| E8 | A Mac with an HDR (XDR) display; HDR10 and HLG clips |
| L0 | Any desktop for the app; a Jellyfin server for that source (optional) |

Two traps from E6: test clips come from `scripts/make-clips.sh` (needs
ffmpeg with libx264), and in cloud sessions the sandbox may refuse to build
or run example code from third-party crates, so read their source instead.

**Terms used below.** *Decoder*: turns compressed video (H.264, HEVC, VP9,
AV1) into raw frames. *NV12*: the raw frame layout most decoders produce, a
full-size brightness plane plus a half-size colour plane. *Colour range*:
whether brightness runs 16 to 235 ("limited", almost all video) or 0 to 255
("full"); getting it wrong makes video look washed out or crushed.
*Surface*: GPUI's element that draws a decoder's frame straight from GPU
memory (macOS only today). *RenderImage*: GPUI's ordinary image, which
needs a CPU conversion and upload for every frame. *Zero-copy*: the frame
goes from decoder to screen without the CPU touching its pixels.
*A/V sync*: showing each frame when its timestamp matches the audio clock.

## 1. Why this exists

GPUI has no video player, and every GPUI app that shows media hits the same
wall. gpui-av is meant to be the general-purpose answer: whatever media an
app wants to play, not one app's needs. Matty (a Mattermost client on GPUI)
is the first user, because it needs video attachments, but it does not set
the limits of the crate. The crates that exist today are thin and each is locked to one old GPUI
snapshot (section 3.3).

The goal is a media layer for GPUI that feels like it belongs there: one typed
Rust API, the platform's own decoders underneath, and frames that reach the
GPU with as little copying as each platform allows.

## 2. Goals and non-goals

Goals, in order:

1. Play the media people actually have, on macOS, Linux and the web: common
   containers (MP4, MOV, WebM, MKV) and codecs (H.264, HEVC, VP9, AV1),
   short clips and long films, with play, pause, seek, volume, mute and
   loop, and sound and picture in sync. The first milestone is Matty's
   video attachments.
2. A small public API that does not leak platform types and has no `unsafe`
   in it.
3. Measure everything. Every claim about smoothness, CPU, memory or battery
   comes with a number from `experiments/`.
4. Stay usable across GPUI snapshot bumps (section 6, risk R1).

Windows joined the goals on 2026-11-15 (section 5.3): gpui-av is meant
for all of GPUI's platforms. Non-goals for now: camera, microphone or screen
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
  every app patches GPUI itself. We do both: section 4.3.
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
  decode, audio output and the clock, and reports state. Backends are
  pluggable: each platform has a default, and an app can pick another one
  (or bring its own) through the public `Backend` trait, without platform
  types or `unsafe` in that trait. Backends:
  - macOS: `AVPlayer` + `AVPlayerItemVideoOutput`, giving `CVPixelBuffer`s.
  - Linux: GStreamer `playbin` with an `appsink`, loaded at run time so the
    app still builds and starts without GStreamer; without it every source
    fails as `Unsupported` (what to do then, such as opening the system
    player, is the app's choice).
  - Web: an `HTMLVideoElement`. Later, WebCodecs `VideoDecoder` plus a Rust
    MP4 demuxer for frames inside the canvas. Mediabunny (a demuxer and
    WebCodecs wrapper) is a candidate for that later web backend; not
    looked at yet.
- **Presenter.** Picks the cheapest path the platform has: `surface` on
  macOS; `RenderImage` on Linux; a DOM `<video>` placed over the element's
  bounds on the web (first cut; menus cannot draw over it). Zero-copy
  presenters for wgpu are a research track (section 5, E5), not a dependency.
- **Clock.** Audio is the master clock wherever the backend gives us one;
  video frames are shown when their timestamp is due and dropped when late.

### 4.1 API sketch (draft)

```rust
let player = cx.new(|cx| VideoPlayer::new(Source::url(url).header("Authorization", token), cx));
player.update(cx, |p, _| p.play());

// In render:
video(&player).object_fit(ObjectFit::Contain).size_full()

// State and events
enum PlaybackState { Loading, Ready, Playing, Paused, Buffering, Ended, Failed(Error) }
cx.subscribe(&player, |this, player, event: &PlayerEvent, cx| { /* ... */ });
```

Commands: `play`, `pause`, `seek(Duration)`, `set_volume(f32)`, `set_muted`,
`set_looping`. Queries: `position`, `duration`, `natural_size`, `state`,
`info`, `buffered`. Events: `StateChanged`,
`InfoChanged`, `Seeked`. Controls (play button, timeline, volume, fullscreen) live in gpuicn,
not here, so gpui-av stays a media crate.

### 4.2 How the crates fit (as built)

```
crates/gpui-av                the API; depends on the GPUI core crate only
crates/gpui-av-avfoundation   macOS backend (E1), depends on gpui-av
crates/gpui-av-web            web backend (E3), depends on gpui-av
crates/gpui-av-gstreamer      Linux backend (E2), depends on gpui-av
crates/gpui-av-mediafoundation Windows backend (S6), depends on gpui-av
crates/gpui-av-platform       the platform's backends in one call (S2)
crates/gpui-av-remux          Matroska and WebM to fragmented MP4 (E7), no deps
lab/                          gpui-av-lab (L0), with its test-pattern backend
```

- **Backends depend on the API crate, not the other way round.** `gpui-av`
  knows no backend. An app picks one at startup with
  `gpui_av::set_default_backend`, or per player with
  `VideoPlayer::with_backend`; a player created with no backend set starts
  in `Failed`. `gpui-av-platform` is the facade that picks for the app:
  `gpui_av_platform::init(cx)` sets a `Fallback` over
  `gpui_av_platform::backends()`, which on macOS is AVFoundation then
  GStreamer, on Windows Media Foundation then GStreamer, on Linux and
  other desktops GStreamer, and in a
  browser the `<video>` backend without the service worker (an app that
  needs request headers there sets `WebBackend::with_service_worker`
  itself). Each backend is a cargo feature, on by default. On macOS
  GStreamer is looked for on the loader's path and then in Homebrew's,
  MacPorts' and the official framework's library folders; GStreamer on
  macOS and Windows builds and is checked to compile, but has not been
  played on those systems yet.
- **`Backend` is a factory, `Session` is one piece of media.**
  `Backend::open(source, events, cx)` returns a `Session`, which takes the
  commands (`play`, `pause`, `seek`, `set_volume`, `set_muted`,
  `set_looping`), answers `position`, and reports everything else (media
  info, state changes, finished seeks) through an `EventSink` that works
  from any thread. The backend is the source of truth for
  `PlaybackState`; the player mirrors it.
- **Backends can be chained.** `gpui_av::Fallback` is a backend made of
  other backends, tried in order: one that refuses a source as
  `Unsupported` (at once, or while loading, before the media is ready)
  passes it to the next, which gets the commands given so far (play or
  pause, seek, volume, mute, looping) again. Any other failure is final,
  since a missing file or a refused token fails on every backend alike,
  and a backend that has the media ready keeps it. Media info is held
  back until then, so a player never shows a size from a backend that
  gave up. When every backend refuses, the error names each one and its
  reason. `Session::backend_name` and `Session::presenter` let a session
  say which backend actually plays, and `VideoPlayer::backend_name`
  reports that. This is how an app plays media its platform's own
  decoder will not open (MKV on macOS through GStreamer, for example)
  without code of its own.
- **The presenter is `Session::paint`.** The `video()` element works out
  where the picture goes (object fit, from the reported natural size) and
  hands the session a `Placement` (the picture's bounds and the clip) plus
  GPUI's `Window`. The session presents however its platform does it best:
  `paint_surface` on macOS, `paint_image` for CPU frames, moving a DOM
  `<video>` on the web. That keeps `CVPixelBuffer` and browser types out of
  the trait, and it is why a backend written outside the crate can draw
  frames at all. While the state is `Playing` or `Buffering` the element
  asks for a new window frame after every paint, and the session decides
  which decoded frame is due. A session whose platform shows the frames by
  itself says so (`Session::needs_frames` returns `false`, as the web
  backend's does): the window then draws only when something else
  changes, and an app that shows the position redraws on its own timer.
- **A failure at open is an event too.** When a backend refuses a source
  outright (or no backend is set), the player starts in `Failed` and a
  `StateChanged` with that failure follows once the code that created the
  player has had the chance to subscribe, so an app handles one path
  whether a failure is found at once or later.
- **A session knows when its picture shows.** `Session::mount` is called
  when the `video()` element starts showing the picture (it is on screen,
  has an area, and the picture's size is known) and returns a guard that
  the element keeps in GPUI's element state. GPUI drops that state at the
  end of the first frame that does not draw the element, so the guard's
  `Drop` is where a session takes down what it presents outside the
  window's scene: the web backend hides its `<video>` there. A session is
  dropped with its player, which covers the player going away.
  `Placement::clip` is the part of the element the window lets show (a
  scrolling parent may cut it); what a session paints into the window is
  clipped already, and an overlay clips itself to it.
- **Buffered ranges.** `Session::buffered` lists the stretches that can
  play without waiting for data, for a timeline to draw; empty means the
  session cannot tell. A local file is buffered from start to end. macOS
  reads `AVPlayerItem.loadedTimeRanges`, the web the element's
  `buffered`, and GStreamer playbin's buffering query (in time, or in
  percent of the length). The lab shows them in its stats panel.
- **Sidecar subtitles.** `gpui_av::Subtitles::parse` reads a sidecar WebVTT or
  SubRip file (telling them apart by WebVTT's header; notes, styles,
  regions and cue settings are skipped, inline markup is stripped) into
  cues sorted by start, and answers which cues show at a position.
  `captions(&player).subtitles(&subtitles)` is a plain element that shows the cue
  due at the player's position at the bottom of its parent, styleable
  like a `div`. It needs no backend support and works on every backend
  that draws into the window; it follows the position because the window
  draws every frame while a picture plays. On the web GPUI cannot draw
  over the `<video>` overlay, so there captions need the element's own
  text tracks (not built yet). The lab loads a `.vtt` or `.srt` with the
  same name as a clip file and shows it over the picture.
- **Tracks inside the media.** `MediaInfo::tracks` lists the audio and
  text tracks (kind, index among its kind, language and title as the
  container states them), with `audio_track` and `text_track` saying
  which play. `VideoPlayer::select_audio_track(index)` and
  `select_text_track(Option<index>)` choose (`Session::select_track`; a
  session that cannot switch ignores it), and the next `InfoChanged`
  carries the result. Text inside the media starts off on every backend.
  A chosen text track's cue reaches the app as text
  (`Session::text`, `VideoPlayer::text`), never burned into the picture,
  and `captions(&player)` draws it, so in-media and sidecar subtitles
  look the same and can be styled. GStreamer: playbin's `current-audio`
  and `current-text`, tags from `get-audio-tags`/`get-text-tags`, and
  playbin's `text-sink` set to an appsink read on the 20 ms poll (with
  `GST_PLAY_FLAG_NATIVE_VIDEO` playbin does not overlay text itself).
  macOS (`AVMediaSelectionGroup`), Windows (`IMFMediaEngineEx` stream
  selection) and the web (`audioTracks`, `textTracks`) list no tracks
  yet.
- **Stats.** `Session::stats` reports frames presented and dropped (zero
  for a session that cannot tell); `VideoPlayer::stats` passes them on for
  the lab's stats panel and bench, and keeps the last counts after a
  failure. `Backend::presenter` names how frames reach the screen
  (`surface`, `image`, `overlay`).
- **The lab (L0, first part).** `lab/` is `gpui-av-lab`: a clip list (files,
  folders and URLs from the command line, plus built-in patterns), a
  backend switcher, the player with transport buttons, and a stats panel.
  The stats sit beside the picture, never over it, because on the web GPUI
  cannot draw on top of the video. Its test-pattern backend
  (`lab/src/test_pattern.rs`) uses only the public API: it draws a
  synthetic picture with `paint_image` against the wall clock, and takes
  sources such as `pattern://1920x1080?fps=60&duration=20`. The only
  platform `cfg` in the lab's view code is the list of backends
  (`lab/src/backends.rs`).
- **The bench (L0, second part).** `corpus/tier-a.json` is the manifest of
  the synthetic tier A corpus that `scripts/make-clips.sh` generates: each
  clip's size as shown, rotation, frame rate, duration, codecs, colour
  range, matrix and transfer function, and whether it must play or fail
  (and with which error kinds). `gpui-av-lab --bench corpus/tier-a.json` plays every clip in the
  lab's own window, muted, and writes JSON per clip and backend: outcome,
  time to first frame, seek time, frames shown and dropped, frame rate,
  drift, CPU, memory, what the backend reported, and how that compares
  with the manifest (match, mismatch, or unknown when the backend cannot
  tell). `--base-url` plays the same corpus over HTTP with `--header`.
  `--soak <seconds>` loops one clip and samples memory and CPU every five
  seconds (criterion 3). The process exits 0 only when everything passed.
- **Drift.** `Stats::max_drift` is the largest gap a session has seen
  between a frame's timestamp and its clock at the moment the frame was
  drawn; `None` when the session does not measure it.
- **macOS (E1).** GPUI's surface takes only full-range NV12 and converts
  with a fixed BT.601 matrix, so the AVFoundation backend asks
  `AVPlayerItemVideoOutput` to convert each frame to exactly that
  (`420f`, BT.601 colour properties). Rotated tracks go through an
  `AVVideoComposition`, since `paint_surface` cannot rotate. A URL with
  request headers loads through the backend's own
  `AVAssetResourceLoaderDelegate` (a made-up scheme, `NSURLSession` range
  requests carrying the headers): headers go to the source's origin only,
  so a redirect to another host gets none, and the headerless URL keeps
  AVPlayer's own loading. HLS with headers fails as `Unsupported` for now
  (section 6, R6). AVFoundation keeps only the low 16 bits of a delegate's
  error code, so the loader's HTTP statuses travel as codes -31000 minus
  the status.
  HDR media comes out of that conversion as a tone-mapped standard range
  picture, which is correct but not HDR: measured 2026-11-13 on a 4K
  59.94 fps 10-bit AV1 HDR10 file (PQ, BT.2020), a frame from this path
  matches Apple's own standard range rendition within 2 of 255 at six
  points, at 59.5 fps with no skipped frames and 7% of a core (without a
  window, so GPUI's drawing is not in that). A browser shows the same
  file in real HDR. GPUI's Metal layer is 8-bit without extended range
  and its surface takes only 8-bit NV12, so real HDR output on macOS
  needs either that changed in GPUI or a presenter of our own that lets
  macOS draw the video (E8). The GPUI change is the chosen route
  (section 4.3, patches 1 and 2); until it lands, standard range is the
  only output on macOS. The media info reports the transfer function
  (`Sdr`, `Pq`, `Hlg`), read from the track's format description, so an
  app can tell which files are affected.
- **Linux (E2).** `gpui-av-gstreamer` loads GLib and GStreamer 1.x with
  `dlopen` when the first source opens (a hand-written table of the few
  functions it calls), so it links nothing at build time; the crate
  builds on every desktop target and is tried on Linux only.
  `GStreamerBackend::is_available` tells an app whether GStreamer is
  there. The session is a `playbin` whose video sink is
  `videoflip video-direction=auto ! videoconvert n-threads=0 ! appsink`
  with BGRA caps: the rotation tag is applied before the frame reaches
  us, and the conversion uses every core to keep its latency short. The
  appsink's callback copies each frame into a `RenderImage` on
  GStreamer's thread and keeps only the newest; `paint` draws it with
  `paint_image` and frees the previous one. The appsink syncs to the
  pipeline clock (the audio clock), so a frame arrives when it is due;
  frames replaced before a paint, and those the sink drops for being late
  (QoS), count as dropped. With the `karya-gpui` feature (our GPUI) the
  appsink asks for NV12 instead and each sample, still mapped, becomes a
  `gpui::VideoFrame` that `paint_video_frame` draws, with no conversion
  or copy on the CPU (E5 step 1; `presenter()` reports "surface"). State comes from the bus, read every 20 ms on
  the main thread, as macOS polls AVPlayer. Request headers go into
  `souphttpsrc`'s `extra-headers` from playbin's `source-setup`; a source
  element without that property fails the media as `Unsupported`.
  libsoup sends them to the source's host and drops `Authorization` on a
  redirect to another host (checked with `scripts/serve.py`'s `/hop/`);
  other headers cross like a browser's would without CORS limits.
  Errors map from GStreamer's error domains, with the HTTP status read
  from souphttpsrc's debug text. GStreamer's demuxers end a file that is
  cut off without an error (the pipeline only says so when the clock
  reaches the stated length), so a picture that ends more than a second
  before the stated length fails as `Decode`. Colour comes from the
  decoder's caps (`colorimetry`), and is left out when the stream does
  not state it. Formats are whatever plugins the machine has: with the
  usual distribution packages that includes VP9, AV1, MKV and WebM, and
  FFV1 through gst-libav.
- **Windows (S6).** `gpui-av-mediafoundation` plays with Media
  Foundation's media engine (`IMFMediaEngine`, which also sits under the
  browser's `<video>` on Windows) in frame-server mode: the engine
  decodes (in hardware where it can), plays the sound and keeps the
  clock; on each paint `OnVideoStreamTick` says whether a new frame is
  due, and `TransferVideoFrame` copies it into a WIC bitmap in BGRA,
  which becomes a GPUI image (the same image path as Linux before E5).
  Engine events arrive on its own threads and go to the main thread
  through a channel. Errors map from the engine's code and the HRESULT
  behind it (file and HTTP statuses). Sources with request headers fail
  as `Unsupported` for now, so a `Fallback` passes them on; sending them
  needs our own `IMFByteStream`. Not reported yet: dropped frames,
  colour, rotation. Zero-copy (a D3D11 texture shared with GPUI's
  renderer) is the later step, as E5 is for Linux. Its tests run on a
  Windows runner in GitHub Actions, only when this crate changes or by
  hand (`.github/workflows/windows.yml`), never as part of `ci`.
- **The lab on Linux.** The lab opens GPUI windows on X11 and Wayland and
  defaults to the GStreamer backend. In the cloud sandbox it runs under
  Xvfb with Mesa's llvmpipe, which draws about 21 window frames a second:
  enough for correctness, not for frame counts or CPU.
- **The web (E3).** `gpui-av-web` plays a URL with one `HTMLVideoElement`
  per session, appended to the page and kept over the element's bounds
  with `position: fixed`: `paint` turns the `Placement` into a CSS box and
  a `clip-path`, measured against the window's canvas (found through the
  window's raw handle), and touches the style only when it changes. GPUI
  paints only when its own window changes, so the session also re-places
  the element when the page scrolls or resizes around the canvas. The
  element lets pointer events through to GPUI. The browser decodes, keeps
  audio and video in step and presents frames on its own, so
  `Stats::max_drift` is `None` and the frame counts come from
  `getVideoPlaybackQuality()`. Limits: GPUI cannot draw over the picture,
  and the overlay is not part of a canvas put into full screen.
  - *Request headers.* A `<video>` element cannot send them. With
    `WebBackend::with_service_worker(url)`, a source with headers plays
    from a made-up same-origin URL that a service worker
    (`gpui_av_web::SERVICE_WORKER_JS`, served by the app next to its page)
    answers by fetching the real URL with the headers and the element's
    `Range`. The worker keeps headers in memory only and asks the page for
    them again after the browser restarts it. Without the worker
    (`WebBackend::new()`, or a browser that will not run it) such a source
    fails with `Unsupported`; headers are never dropped. The real server
    must allow the page's origin (CORS) when it is another origin. On a
    redirect to another origin the browser drops `Authorization` by
    itself (checked in Chromium 153 with the test server's `/hop/` route:
    the clip plays and the token never reaches the second host); any
    other header crosses only if that origin's CORS preflight allows it.
  - *Errors.* A media element reports a 404, a 401 and an unknown format
    all as "source not supported", so on an error the backend requests
    the first byte of the URL itself and maps the HTTP status:
    `NotFound`, `Unauthorized`, `Network`, and otherwise `Unsupported` or
    `Decode` from the element's own code.
  - *What a browser does not tell.* Colour range and matrix, and the
    rotation tag (the browser applies it; the reported size is the
    rotated one). Whether there is an audio track comes from the track
    list (Safari), Firefox's flag, or Chrome's count of decoded audio
    bytes.
  - *Autoplay.* A browser refuses audible playback that no click or key
    press led to; the session then reports `Paused` and logs why.
  - *Cross-origin isolation.* GPUI's web build needs an isolated page
    (COOP and COEP headers), under which another origin's media must be
    served with CORS or `Cross-Origin-Resource-Policy`; the backend asks
    for CORS when the page is isolated.
- **The lab on the web.** `scripts/web serve [folder]` builds the lab for
  `wasm32-unknown-unknown` (nightly toolchain, as gpuicn and Matty) and
  serves it with `scripts/serve.py`, which also serves a folder of media
  with range requests, and the same media under `/private/` behind a
  bearer token. For redirects it has `/moved/` (to `/private/` on the
  same host), `/hop/` (to `/open/` under the server's other host name)
  and `/open/`, which refuses a request that still carries
  `Authorization`, as a signed storage URL does. The page's query string is the command line
  (`?bench=tier-a.json&base-url=clips/`, `?clip=<url>`, `?muted`); with no
  clip it lists the served folder, so any folder of videos can be played.
  An unattended run leaves its JSON on `window.gpuiAvLab` and offers it as
  a download. The lab's stage has two more arrangements for testing a
  presenter outside the scene: `Scroll` (the picture in a scrolling
  column) and `Hide` (the element gone, the player alive). The lab's
  entry (`lab/src/entry_native.rs`, `entry_web.rs`) and its backend list
  are the only code that differs per platform.
- **Media info** carries duration, natural size (as shown, rotation
  applied), whether there is video, and as optional values the rotation
  tag, whether there is audio, and colour range, matrix and transfer
  function (standard range, PQ or HLG, so an app can label HDR media): a
  backend leaves out what its decoder did not report, and never guesses.
  The transfer function describes the media, not the picture on screen.

### 4.3 Our GPUI (karyalabs/zed)

Goal: gpui-av adds no latency of its own, and GPUI is never the reason a
platform gets a slower or poorer picture. Several things gpui-av needs are
renderer or core changes that we do not own (section 3.1), so Karya Labs
keeps its own GPUI.

**What gpui-pre is.** Zed stopped publishing `gpui` to crates.io at 0.2.2
(October 2025). `gpui-pre` is a weekly republish of Zed's GPUI crates by
huacnlee (Longbridge, author of gpui-component), named `gpui-pre`,
`gpui-pre-apple` (Metal renderer), `gpui-pre-wgpu`, `gpui-pre-macos` and
so on; 0.3.12 is Zed at `d2a4fb6`. gpui-av, gpuicn, Matty and gpui-kit
(gpui-base, gpui-component) all build on it.

**Shape.**

- Source of truth: `karyalabs/zed`, a GitHub fork of `zed-industries/zed`.
  For each gpui-pre snapshot we pin, a branch starts at the same Zed
  commit and carries our patches on top. Real history keeps the weekly
  rebase cheap and lets every patch go to Zed as a pull request.
- Output: a script turns that branch into crates with **the gpui-pre names
  and the same version**, the way gpui-pre itself is made. Apps opt in with
  one `[patch.crates-io]` block; gpui-kit, gpuicn and Matty then build on
  it unchanged. A fork under its own crate names would split every app off
  from gpui-kit, so we never rename.
- Patches are additive: new pixel formats, new surface kinds, opt-in modes.
  Nothing that builds on gpui-pre may break on ours.
- gpui-av builds on stock gpui-pre and keeps today's paths there. The fast
  paths sit behind the cargo feature `karya-gpui`, which an app turns on
  together with the patch. No user is forced onto our GPUI.
- Karya Labs repos use our GPUI by default (Arya, 2026-11-13): each
  workspace's `[patch.crates-io]` takes the changed crates from the
  `crates/gpui-pre-<version>` branch of karyalabs/zed, which
  `karya/publish.sh` there regenerates after a patch merges. In this repo
  the lab turns `karya-gpui` on by default, and `scripts/karya-gpui` runs
  any cargo command against another karyalabs/zed branch.
- Every patch is also proposed to Zed. When Zed merges it, we drop it, so
  the fork shrinks over time.
- CI rebuilds the patch set against each new gpui-pre snapshot (the canary
  of R1) and runs the lab's bench on both stock and patched GPUI.

**Patch list, in order.** Each item is tried as an experiment first.

1. macOS video formats (`gpui-pre-apple`): accept 10-bit P010 and
   limited-range NV12, and take matrix, range and transfer from the
   frame's attachments instead of the shader's fixed BT.601 full range.
   This also removes our conversion pass (section 7, 2026-11-13).
2. macOS HDR (`gpui-pre-apple`): an EDR mode, on only while HDR video is
   on screen: CAMetalLayer in RGBA16Float with an extended colour space
   and `wantsExtendedDynamicRangeContent`, every pipeline built for that
   format (about nine are fixed to BGRA8Unorm today), and PQ/HLG mapped to
   the screen's EDR headroom in the video shader.
3. A cross-platform surface type in core: today `PaintSurface` holds a
   CVPixelBuffer and exists on macOS and iOS only.
4. Linux video (`gpui-pre-wgpu`), in two steps. Step 1, done
   (karyalabs/zed#7): `gpui::VideoFrame` holds NV12 in memory the caller
   owns and `Window::paint_video_frame` draws it; the renderer uploads the
   planes and converts with the frame's own matrix and range. Step 2, written
   and not yet measured (karyalabs/zed#8): draw from dmabuf planes through
   wgpu's `texture_from_dmabuf_fd`, so VA-API frames show with no copy.
   Then P010 for 10-bit (E5).
5. Web: draw a `VideoFrame` or `HTMLVideoElement` inside the canvas
   through WebGPU external textures once wgpu releases them, which removes
   the DOM overlay's limits (E4).
6. Frame pacing: present at the video's own rate and report when a frame
   actually reached the screen, for A/V sync and latency numbers. Found on
   the A10 (2026-11-14): GPUI throttles a window it thinks is inactive to
   30 fps by default (`WindowOptions::inactive_frame_interval`), which
   halves 60 fps video in any window without focus; GPUI's X11 refresh
   rate ignores DoubleScan modes; and our element asks for a frame every
   tick while it waits for the next video frame.

Inside gpui-av, the element already stops asking for frames when the
backend does not need them (`Session::needs_frames`); idle cost is ours to
fix and needs no GPUI change.

## 5. Experiments

Each experiment is a folder under `experiments/` with a README holding the
question, the setup, the result and the numbers. An experiment answers one
question; when it is done, the answer is folded into this spec.

| ID | Question | Done when |
|---|---|---|
| E0 | What do real Mattermost videos look like (containers, codecs, sizes, lengths)? | A table from a sample of real attachments |
| E1 | macOS: does AVPlayer + `surface()` play 1080p60 smoothly inside a GPUI window? | A demo plays with sound; CPU and dropped frames measured. **Done 2026-11-13: yes**, 0 of 3600 frames dropped at ~19% of one core. See `experiments/e1-avfoundation` |
| E2 | Linux: GStreamer + RenderImage at 1080p: what does the per-frame copy cost? | CPU, memory and dropped frames at 720p and 1080p. **Done 2026-11-13 (sandbox, no GPU): tier A passes from files and over HTTP; the copies cost about 1.9 ms of CPU per 720p frame and 5 to 6 ms per 1080p frame, as much as software decoding. On an NVIDIA A10 (2026-11-14): tier A 15/15 and 4K/AV1/VP9 5/5 with NVDEC and in software; no drops up to 1080p60, flat memory over a 10 minute 1080p60 soak (0.4% dropped, 142% CPU); 4K 10-bit drops a third of its frames through the image path.** Wayland and VA-API on Intel/AMD are a listed gap. See `experiments/e2-gstreamer` |
| E3 | Web: a DOM `<video>` overlay that tracks a GPUI element through scroll, resize and close | Works in Chrome, Firefox and Safari. **Done for Chrome 2026-11-13:** tier A passes, memory flat over 10 minutes, overlay follows resize, scroll and a window moved between a 2x and a 1x screen. Safari and Firefox are not run: a listed gap. See `experiments/e3-web-overlay` |
| E4 | Web: WebCodecs frames drawn inside the canvas without an overlay | One clip plays in Chrome; cost measured against E3 |
| E5 | Zero-copy on wgpu: implement `PrimitiveBatch::Surfaces` (NV12) in gpui-pre-wgpu | Patch works locally; decide whether to propose it to Zed. **Step 1 done 2026-11-14 (karyalabs/zed#7, NV12 from memory): on the A10 CPU falls by half to three quarters with NVDEC (1080p60 158% to 52%, 4K 10-bit HEVC 380% to 128% and no drops), copies 0.01 ms a frame.** Step 2 (dmabuf, no copy out of video memory) is written (karyalabs/zed#8, `GPUI_AV_DMABUF=1`) but unmeasured: on OVH's NVIDIA image no VA-API decoder runs (2026-11-15, L4), so it waits for an Intel or AMD Linux desktop. See `experiments/e2-gstreamer` |
| E6 | Can we build on gpui-video-player or alhazen instead of writing backends? | **Done 2026-11-12: no; reuse ideas.** See `experiments/e6-existing-crates` |
| E7 | Containers the platform will not open (MKV on AVFoundation and in browsers): our own Rust demuxer feeding VideoToolbox / WebCodecs (Mediabunny on the web), or an optional GStreamer backend on macOS? | The MKV clips in the corpus play on macOS and in Chrome through the chosen path; cost measured. **Path chosen 2026-11-15: remux to fragmented MP4 in pure Rust (`gpui-av-remux`) and keep the platform player, fed through an HLS playlist on macOS and Media Source Extensions on the web.** In the sandbox every frame comes through, decoded video matches the source frame for frame, A/V sync is kept to the millisecond, and 30 MB remux in 0.08 s. In Chromium through MSE every Matroska and WebM clip it has decoders for plays and seeks; the AVFoundation HLS path and Safari and Firefox wait on the Mac check. See `experiments/e7-mkv-remux` |
| E8 | macOS: real HDR output. Route chosen 2026-11-13: an extended range path in our GPUI (section 4.3, patches 1 and 2). The alternative, a second presenter that lets macOS draw the video in its own layer over the element (`AVSampleBufferDisplayLayer` or `AVPlayerLayer`, placed as the web overlay is; GPUI cannot draw over it), is not scheduled. | An HDR10 and an HLG clip show with extended brightness on an XDR display through the chosen path; cost and limits recorded |
| L0 | Build `gpui-av-lab`, the playground and benchmark app (section 5.1) | Interactive mode and `--bench` play the tier A corpus from local and HTTP sources. **Done for tier A 2026-11-13**, natively and in a browser |

Order: E0 and E6 first (cheap, and they can cancel work), then E1 and E3
(the two biggest user groups after the web), then E2, E4 and E5. E7 follows E1
and E4, since it reuses their decode paths. L0 starts
with E1: its first backend is the E1 code, and every later experiment
reports its numbers through `--bench`.

E1, E3 and that first slice of L0 together make the proof of concept
(section 5.2), which came before everything else and is done
(2026-11-13).

Test clips come from `scripts/make-clips.sh` (ffmpeg's `testsrc2` and `sine`
sources) rather than being committed; the repo ignores video files.

### 5.1 The playground: `gpui-av-lab`

One app in this repo (`lab/`) is where gpui-av gets tried and measured.
Because gpui-av is general purpose, the lab tests the whole range of media
people have, not only Matty's. It is still a test bench, not a product: no
home screens, posters, resume or watch history. Features that belong in the
crate (audio track and subtitle selection, when they arrive) do get a plain
picker in the lab so they can be tested.

- **Interactive mode.** A clip list, the player, and a stats overlay:
  backend, presenter, fps, dropped frames, A/V drift, colour range and
  matrix. The backend can be switched at run time, which also exercises the
  pluggable `Backend` trait.
- **`--bench` mode.** Plays a list of clips without a person and writes JSON
  per clip and backend: time to first frame, seek time, CPU, memory, dropped
  frames, A/V drift, and whether the reported metadata matched the
  manifest. Experiment READMEs quote these numbers.
- **Sources** are pluggable too: a local folder; plain HTTP with request
  headers (the Matty case: authenticated file URLs with range requests);
  and a Jellyfin server, which is a first-class source and the main home
  of the large corpus. The lab lists a Jellyfin library with filters by
  container, codec, resolution and HDR, taken from the metadata Jellyfin
  already probed for each file; that metadata also serves as the expected
  values for the bench, so the large corpus needs no hand-written manifest.
  Jellyfin is only used for direct play (`/Videos/{id}/stream?static=true`
  with its token header), and the test user has transcoding and remuxing
  turned off, so a failure shows our decoder coverage instead of being
  hidden by the server. HLS stays a non-goal for now.

**Corpus.** A manifest (in the repo) lists every clip with its expected
container, codec, size, frame rate, colour range, matrix, transfer,
rotation and duration, so the bench checks correctness as well as speed.
No video files are committed: synthetic clips come from
`scripts/make-clips.sh`, and public clips (Blender open films and similar)
are listed by URL and SHA-256 and fetched by a script
(`scripts/fetch-real-clips.sh`). The synthetic clips serve the bench; the
real films, with real picture and sound, serve people judging smoothness,
lip sync and colour by eye and ear (Arya, 2026-11-12), and any folder of
your own videos opens in the lab the same way. Arya hosts the same
files in Jellyfin.

Both tiers are first-class; tier A is listed first only because it is the
first milestone.

- **Tier A, Matty-like (first milestone).** Short MP4 and MOV from
  phones and screen recorders: H.264 and HEVC, portrait and rotation tags,
  variable frame rate, no audio, audio only, moov at the end vs faststart.
  E0 refines this tier from real attachments.
- **Tier B, coverage.** VP9 and AV1 (WebM and MP4), 4K and 60 fps, 10-bit,
  HDR10 and HLG, full range, BT.601/709/2020, odd sizes, non-square pixels,
  fragmented MP4, MKV, several audio tracks and 5.1, long files for seeking,
  truncated and corrupt files. A clip that fails on a backend is a coverage
  gap to close, not out of scope: AVFoundation and browsers do not open MKV,
  which E7 addresses. The bench reports a per-backend coverage table so the
  gaps stay visible.

A full Jellyfin client could be a showcase later, once gpui-av and the
gpuicn controls are stable. It is not the playground.

### 5.2 The proof of concept (POC)

Status: agreed 2026-11-12, done 2026-11-13. The workspace, the API crate,
the macOS backend, the interactive lab with its test-pattern backend, the
bench, and the web backend with the lab's web build are in (section 4.2).
Arya played their own videos in both and accepted it by eye and ear.
Chrome measured 2026-11-12 on the same Mac (Chrome 157, in a throwaway
profile, muted; tables in `experiments/e3-web-overlay`), against the
criteria below:
1. 1080p60 for 60 s: 0 of 3,569 frames dropped by Chrome's own count,
   same origin and through the header service worker. Met.
2. First frame 9 to 27 ms (to the first frame Chrome counts); seeks 14 to
   35 ms. Met.
3. 10 minute loop (on a virtual screen): the page's renderer 242 to
   254 MB, flat after the third minute; GPU process 110 to 112 MB; still
   playing. Met for memory; CPU cannot be read by the page and was not
   noted by hand.
4. Every playable tier A clip plays (H.264, HEVC, MP4, MOV) and matches
   size, duration and tracks; rotation is applied but not reported, and
   colour is unknown, as a browser does not say. Met.
5. Through the service worker every request carried the header and a
   `Range`; without it every clip ends in `Failed(Unauthorized)`. Met.
6. Missing file `NotFound`, truncated `Decode` (after a fix: Chrome ends
   such a file early instead of failing), random bytes and FFV1
   `Unsupported`; no hang. Met.
8. The lab switches between the web and test-pattern backends at run time
   in Chrome (the `<video>` element is removed and restored). Met.
9. The overlay stays aligned through window resizes and a scroll (cut at
   the stage's edges), hides with the element, and lets clicks and the
   wheel through to GPUI. Under an emulated pixel ratio change (headless
   Chrome, 2 to 1 to 1.5 and back) the picture stayed on the stage's
   edges. By hand (Arya, 2026-11-13): the lab's Chrome window dragged
   back and forth between the MacBook's 2x screen and a 1x monitor,
   stopping halfway too, kept the picture in place with no resize. Met.
10. The same lab code runs natively and in the page. Met.
Safari: not run (it could not be opened without disturbing the Mac's
user); Firefox: not installed. The soak and hand checks ran on a virtual
screen (`scripts/virtual-display`); its frame counts are not used, as an
unfocused window's canvas draws at about 26 a second there.

macOS measured 2026-11-12 with the lab's bench (M4 Max, macOS 27.2; tables
in `experiments/e1-avfoundation`), against the criteria below:
1. 1080p60 for 60 s: 0 of about 3,565 frames dropped (file and HTTP); max
   drift 8 to 17 ms against AVPlayer's clock. Met.
2. First frame 30 to 80 ms from a local file, 140 ms for the rotated clip
   (it goes through a video composition) and 190 to 240 ms for the first
   player in a process; seeks 30 to 95 ms, file or local HTTP. Met.
3. 10 minute loop of the 1080p60 clip: resident memory +1.1 MB with no
   trend, CPU 33% of a core (lab window included), 0.82% dropped, all in
   the first and last 5 s. Met; the web half is above.
4. All 11 playable tier A clips match the manifest's size, rotation,
   duration, tracks, colour range and matrix. Met on macOS.
5. Over local HTTP with a bearer token and range requests every clip
   plays and every request carries the header; without it every clip ends
   in `Failed(Unauthorized)`. Met on macOS.
6. Missing file `NotFound`, truncated and random bytes `Decode`, FFV1
   `Unsupported`; no panic, no hang. Met on macOS.
Caveat: runs where macOS throttled a covered lab window drop frames; the
bench records `window_fps` so they can be told apart.

**What it proves.** The architecture in section 4 works end to end on two
platforms that share almost nothing underneath: a real `gpui-av` crate
plays the tier A corpus through one public API, inside `gpui-av-lab`, on
macOS and in a browser, with numbers. In experiment terms the POC is E1,
E3 and the first slice of L0, with the code promoted into the crate instead
of thrown away. The POC is general-purpose in design and narrow in
coverage: nothing in its API may assume Matty, macOS or AVFoundation.

**In scope.**

- **Platforms:** macOS and the web (decided 2026-11-12). Chrome is the
  browser that gates the POC; Safari and Firefox are run and their results
  recorded, and a failure there is a listed gap, not a blocker. Linux is
  the next milestone.
- **Crates:** a Cargo workspace with `gpui-av` (the API: `VideoPlayer`
  entity, `video()` element, `Source`, the public `Backend` trait,
  `PlaybackState`, `PlayerEvent`), the AVFoundation and web backends each
  in their own crate, and `lab/`.
- **Backends and presenters:** on macOS, `AVPlayer` +
  `AVPlayerItemVideoOutput`, drawn with `surface()`. On the web, an
  `HTMLVideoElement` placed as a DOM overlay over the element's bounds.
  The app code that creates the player and the `video()` element is the
  same on both.
- **Pluggability:** a test-pattern backend that lives in the lab, outside
  the crate, and uses only the public `Backend` trait. It
  produces synthetic frames and a clock, and doubles as a test fixture.
- **Sources:** a file path (macOS), and a URL with request headers and
  range requests. A `<video>` element cannot send custom request headers,
  so on the web the POC must settle how a source with headers plays (a
  service worker that adds them is the first candidate) or, failing that,
  refuse such a source with a clear error. It never drops headers
  silently. A Jellyfin direct-play URL with its token header plays through
  that same HTTP source; library browsing, filters and Jellyfin metadata as
  bench expectations follow right after the POC.
- **Commands and queries:** `play`, `pause`, `seek`, `set_volume`,
  `set_muted`, `set_looping`; `position`, `duration`, `natural_size`,
  `state`; `object_fit` on the element.
- **Media:** tier A only. MP4 and MOV with H.264 or HEVC and AAC: landscape
  and portrait, rotation tags, variable frame rate, no audio, audio only,
  moov at the end and faststart. All clips come from
  `scripts/make-clips.sh` and are listed in the manifest.
- **Lab:** the clip list, the player, the stats overlay, the backend
  switcher, and `--bench` writing JSON per clip and backend. The lab also
  builds for the web, where the bench runs in the page and offers the same
  JSON as a download.

**Out of scope.** The Linux backend, WebCodecs frames inside the canvas
(E4), MKV (E7), tier B, zero-copy on wgpu (E5), `buffered`, audio track and subtitle selection, controls UI, the
move into gpuicn, publishing to crates.io, and E0 (the POC uses the
synthetic tier A; E0 refines it later).

**Acceptance criteria.** The POC is done when all of these hold on Arya's
MacBook Pro, natively and in Chrome on the same machine unless a criterion
says otherwise, with the numbers taken from the bench and recorded here.
The thresholds in 1 to 3 are starting guesses; the first measurements may
adjust them, in this file.

1. **Smooth playback.** A 1080p60 H.264 clip plays for 60 seconds with
   under 1% dropped frames and A/V drift within 40 ms. On the web, dropped
   frames come from the browser's own playback-quality counters, and drift
   is the browser's to keep, so only dropped frames are checked there.
2. **Responsiveness.** Time to first frame is under 300 ms from a local
   file on macOS and under 500 ms from the local test server in Chrome; a
   seek settles on the new frame in under 500 ms on both.
3. **Stability.** A 10 minute loop shows no memory growth; CPU and memory
   are recorded on macOS. The page cannot read them on the web, so there
   they are noted by hand from Chrome's task manager.
4. **Correctness.** Every tier A clip plays on both platforms. Reported
   size (as shown, so with the rotation applied) and duration match the
   manifest on both; the rotation tag, colour range and matrix match on
   macOS, and are reported as unknown on the web, where `<video>` does
   not expose them (amended 2026-11-13: the rotation tag joined that
   list; the rotated size still proves the browser applied it).
5. **HTTP.** A source with an auth header and range requests plays from a
   local test server on macOS, and a 401 ends in `Failed`. On the web, the
   same source either plays or is refused with a clear error, as settled
   under Sources above.
6. **Failure.** A missing file, a truncated file and an unsupported codec
   each end in `Failed(Error)` on both platforms: no panic, no hang.
7. **API hygiene.** The `gpui-av` API crate has `#![forbid(unsafe_code)]`
   and no platform types in its public items; `unsafe` and browser types
   live only in backend crates.
8. **Pluggability.** The lab switches between the platform's backend and
   the test-pattern backend at run time, on both platforms.
9. **Overlay.** On the web, the video stays aligned with its GPUI element
   through scroll, window resize and device-pixel-ratio change, and is
   removed when the element or the player goes away. Known limit, recorded
   and accepted for the POC: GPUI content cannot draw over the video.
10. **One app.** The lab's player view is one piece of code with no
    platform `cfg` in it; only backend selection differs per target.
11. **CI.** The workspace builds and its tests pass on macOS and for
    `wasm32-unknown-unknown`, against a pinned gpui-pre snapshot. While
    the crate is early (Arya, 2026-11-13), CI only checks formatting and
    that the workspace compiles on Linux; the macOS tests and the web
    build are run locally before a merge. Heavier CI comes back once
    there is more to protect.

The POC is done (2026-11-13): E1 and E3 are marked done, L0 is marked
done for tier A, and the next milestone is Linux (E2). Gaps carried
forward, none a blocker: Safari and Firefox not run, CPU during the Chrome
soak not noted, and 3 to 4% dropped frames on the variable frame rate
clip in Chrome, not yet explained.

### 5.3 After the POC: the roadmap (from 2026-11-15)

Arya, 2026-11-15: the POC is proven, so gpui-av ships continuously, one
small pull request at a time, merged when CI is green. The aim is unchanged
(section 2): a general-purpose media crate for every GPUI platform that
plays the vast majority of media people have and adds no latency of its
own. The order below puts first what makes the crate usable by anyone, then
what widens the media it plays, then new platforms. Measurement work that
waits on hardware (E5 step 2 needs an Intel or AMD Linux desktop, from
about 2026-11-24) runs beside it and blocks nothing here.

| Step | What ships | Status |
|---|---|---|
| S1 | `Fallback`: backends tried in order, so media one decoder refuses plays through another (section 4.2) | Done 2026-11-15 |
| S2 | A platform default: one crate whose `init(cx)` sets the right `Fallback` for the target (macOS: AVFoundation, then GStreamer where installed; Linux: GStreamer; web: the browser), each backend behind a cargo feature an app can turn off; the lab gains an "auto" entry | Done 2026-11-15; GStreamer not yet played on macOS or Windows |
| S3 | The public API made ready for others: docs on every item with runnable examples, a quick start in the README, `buffered`, and a check that the crates package for crates.io (publishing itself waits for Arya) | `buffered` done 2026-11-15 (macOS part compiled, not yet run on a Mac); `gpui-av` packages and builds alone on stock gpui-pre; a runnable quick start (`gpui-av-platform`'s `play` example, run under Xvfb with GStreamer) and the README's quick start done 2026-11-15; compiled examples on the crate, `VideoPlayer`, `PlayerEvent`, `Source`, `Error`, `video()` and `Subtitles` done 2026-11-15; examples for writing a backend (`Backend`, a whole session in one compiled example), `captions()` and picking tracks by language (`MediaInfo::tracks`) done 2026-11-16 |
| S4 | Subtitles: sidecar WebVTT and SRT parsed in the API crate, cues by position, and an element that draws them over the picture on the native backends (on the web the `<video>` element's own text tracks, since GPUI cannot draw over the overlay) || Sidecar WebVTT and SRT with `captions()` done 2026-11-15 for backends that draw into the window (checked with GStreamer under Xvfb); `VideoPlayer::set_subtitles` done 2026-11-16, the one way to show a subtitle file on every backend: `captions()` draws them on the native backends, and the web backend adds them to the `<video>` element as a text track the browser draws (checked in headless Chromium; the lab fetches a `.vtt` or `.srt` beside a URL) |
| S5 | Audio and text tracks inside the media: list them and pick one, on each backend || GStreamer done 2026-11-15 (list, switch, in-media text through `captions`, checked in the lab under Xvfb); macOS written 2026-11-15 (media selection groups for audio and text, text through a legible output into `captions`; compiles, not yet run on a Mac); tier A gains `h264-1280x720-tracks.mp4` (2 audio, 1 text) and the bench checks the counts on backends that list tracks; Windows lists and switches audio tracks through `IMFMediaEngineEx` stream selection (#28) and text tracks through `IMFTimedText` (#36: in-band text on, cues collected from its notifications into `text()`; checked on a Matroska SubRip clip; Windows' MP4 source drops 3GPP `mov_text` streams entirely, so MP4 subtitles are not listed); web lists the tracks the browser lists (Safari: audio and in-band text; Chrome: audio only behind its `AudioVideoTracks` flag, checked headless; Firefox: none by default), switches audio through `enabled`, and reads the chosen text track's `activeCues` into `text()`, with the track `showing` so the browser draws it (GPUI cannot draw over the video element) |
| S6 | Windows: a Media Foundation backend (`IMFMediaEngine`), drawn as GPUI images first, checked by a Windows run in GitHub Actions started by hand (CI stays one fast job) || Done 2026-11-15 (#24): every engine check passes on `windows-latest`, and the lab's bench gives 13 of 15 in a GPUI window there (after #26). Open: HEVC needs the Store's HEVC extension (absent on the runner), an audio-only file fails with no audio device (the runner has none; should play silently); no request headers yet. A truncated MP4 left the engine's clock running past the stated length with no error; it now fails as `Decode` once the drawn picture has stopped for 4 s (or ten frame intervals, for slow video) while the clock runs, or, with no window, once the clock passes the length (#26) |
| S7 | Wider media: E7 (MKV and WebM on macOS and the web), tier B in the corpus and the bench's coverage table per backend | Tier B and the coverage table done 2026-11-15 (#29): 19 clips from `scripts/make-tier-b.sh` (`corpus/tier-b.json`). GStreamer 1.24 with plugins good, bad and libav plays 18 of 19 under Xvfb; the gap is full-range H.264, which gst-libav decodes and reports as limited range (so its picture is too, an upstream issue). GStreamer without a decoder for the picture or sound now fails as `Unsupported` instead of playing the rest silently. E7's remuxer done 2026-11-15 (`gpui-av-remux`, #30); the AVFoundation backend plays `.mkv` and `.webm` files, and URLs named so (Jellyfin direct play), through it from a local HLS server (written, checked over HTTP with GStreamer and ffmpeg, waiting on the Mac check); the web backend feeds them through Media Source Extensions when the `<video>` element refuses them (headless Chromium: the same 15 of 19 tier B clips pass as natively, seeks included; Safari and Firefox with the Mac check). VP9 on macOS stays a gap (Apple's HLS does not take it). Windows, 2026-11-16 (#40, `windows-latest` runner, Windows Server 2025 without Store codec extensions): tier B 9 of 19 on Media Foundation. H.264 in every container including Matroska, 4K60, anamorphic, full range, BT.601, 5.1 AAC, the 10 minute clip and the corrupt one play. Every VP9, AV1 and HEVC clip fails cleanly as `Unsupported` (no decoder without the VP9, AV1 and HEVC extensions), audio-only files fail for want of an audio device on the runner, and the fragmented MP4 reports a different length. Tier A there is 12 of 16: HEVC, audio-only, the MP4's subtitle track (see S5) and a false stall on the 1080p60 clip in the runner's software rendering. With GStreamer 1.26 installed (the official installer through Chocolatey) the platform's fallback, Media Foundation then GStreamer, plays 17 of 19 tier B clips there: every VP9, AV1 and HEVC clip (10-bit, HDR10 and HLG included) and audio-only Opus. The two left are the fragmented MP4's length and a time-out on the first clip while GStreamer builds its plugin registry on first use. Getting there needed `Fallback` to treat buffering before ready as loading (Media Foundation reports it before it finds it has no decoder), and the GStreamer backend to find the installer's folder, which is not on `PATH`, for itself and for its plugins |

## 6. Risks

- **R1. GPUI churn.** gpui-pre ships a new snapshot about weekly, and the
  existing video crates broke on exactly this. Keep the surface we touch
  small, and run a canary against new snapshots.
- **R2. Renderer ownership.** HDR, zero-copy outside macOS and frame
  pacing need GPUI changes we do not own. We carry them in our own GPUI
  (section 4.3) and propose each to Zed; nothing in goals 1 to 4 depends
  on an app using it. Cost: a weekly rebase onto each gpui-pre snapshot,
  and review time upstream.
- **R3. Small audience.** About 100 to 150 crates depend on GPUI. The aim is
  to serve all of them, not only Matty, so the API and backends are judged
  against general use; Matty is the first proof, not the bar.
- **R4. Codecs and law.** Use only the decoders the OS or browser already
  has; never bundle GStreamer plugins or ffmpeg. Not legal advice.
- **R5. Scope.** One maintainer plus Claude. Keep to the non-goals.
- **R6. Request headers on macOS.** Resolved 2026-11-13. The backend used
  `AVURLAssetHTTPHeaderFieldsKey`, exported but in no public header (App
  Store review has rejected it), with which AVFoundation sent the headers
  on to the target of a cross-host redirect. Sources with headers now load
  through our own `AVAssetResourceLoaderDelegate`; headers go only to the
  source's origin. Cost, tier A over local HTTP: median time to `Playing`
  66 ms against 45 ms with the old option, seeks unchanged (median 82 ms
  against 81). Open: HLS with headers (fails as `Unsupported`).

## 7. Decisions

- 2026-11-15 (E7): Matroska and WebM on AVFoundation and in browsers are
  remuxed to fragmented MP4 by our own pure-Rust crate and played by the
  platform's player (HLS from a localhost server on macOS, MSE on the web),
  rather than demuxed into VideoToolbox or WebCodecs: the platform players
  already own the clock, sync and seeking, and remuxing decodes nothing.
- 2026-11-15: after the POC, gpui-av ships continuously along section 5.3
  (Arya: "we've basically proven the POC works, so we can keep
  shipping"), and Windows is a goal, not a non-goal. Media a platform's
  own decoder refuses falls through to the next backend an app lists
  (`Fallback`), on `Unsupported` only.

- 2026-11-14 (E2, E5): on Linux, gpui-av draws NV12 through our GPUI
  (`karya-gpui`, karyalabs/zed#7) and keeps `RenderImage` as the fallback
  on stock gpui-pre. Measured on an OVH NVIDIA A10: NV12 halves to
  quarters the CPU, removes the 4K 10-bit drops with NVDEC, and needs no
  GPU-specific code. Next is dmabuf import (E5 step 2). Every window that
  shows video sets `inactive_frame_interval: None`: GPUI's default 30 fps
  cap for inactive windows, not the GPU or llvmpipe, was what held earlier
  Linux runs to 21 to 30 fps.

- 2026-11-13 (E2): the Linux backend is GStreamer `playbin` drawn as
  `RenderImage`s, with GStreamer loaded at run time. It plays tier A, but
  its three copies per frame (convert to BGRA, copy into the image, GPUI's
  upload) cost as much CPU as decoding, so zero-copy on wgpu (E5, section
  4.3 patch 4) is the next step for Linux. The tier A manifest gains
  `may_play_on`: a clip that must fail elsewhere may play on the listed
  backends (FFV1 on GStreamer with gst-libav).
- 2026-11-13: the proof of concept (section 5.2) is done. Arya accepted
  playback by eye and ear with their own videos and checked the web
  overlay across two screens by hand. Next: Linux (E2).
- 2026-11-13: Karya Labs keeps its own GPUI (section 4.3): a fork of Zed
  shipped under the gpui-pre crate names and versions, additive patches
  only, opted into with `[patch.crates-io]`, each patch also proposed to
  Zed. gpui-av keeps working on stock gpui-pre. Chosen over patching only
  the Metal renderer (too small: Linux and web need core changes) and over
  a renamed fork (it would split apps off from gpui-kit, gpuicn and Matty).
  Reason (Arya): gpui-av should add no latency and work for nearly anyone,
  and GPUI must not become the bottleneck on any platform.
- 2026-11-13 (E8): real HDR output on macOS comes through our GPUI's
  extended range patches (section 4.3), not through a native-layer
  presenter in the backend. Until then HDR media shows as a correct
  standard range picture.
- 2026-11-13 (R6): on macOS a source with request headers loads through
  our own `AVAssetResourceLoaderDelegate`; headers go to the source's
  origin only. `AVURLAssetHTTPHeaderFieldsKey` is gone.
- 2026-11-13 (E1): the macOS backend is AVPlayer + AVPlayerItemVideoOutput
  drawn with `paint_surface`, converting frames to full-range NV12 with the
  BT.601 matrix that GPUI's shader assumes; `MediaInfo.color` still reports
  the file's own range and matrix. Letting GPUI's surface take limited-range
  and BT.709 frames is an upstream candidate that would remove the
  conversion.
- 2026-11-13 (E1): `Session` gains `stats()` (frames presented and dropped).

- 2026-11-12: gpui-av is a general-purpose media crate for GPUI. Matty is
  its first user, not its scope; coverage gaps such as MKV on macOS and the
  web count as real gaps (E7).
- 2026-11-12: the playground is `gpui-av-lab` (section 5.1), an interactive
  and `--bench` app with pluggable sources, Jellyfin among them in direct
  play only, as a first-class source for the large corpus. Not a full
  Jellyfin client (home screens, posters, resume): that is a second product
  for one maintainer, and the lab gets the same coverage without it.
- 2026-11-12: backends are pluggable and selectable by the app, with a
  default per platform.

- 2026-11-12: the proof of concept (section 5.2) covers macOS and the web
  (Arya). Two platforms with nothing in common underneath are the real
  test of one API; Linux follows. Pluggable backends are proved by a
  test-pattern backend that lives in the lab and uses only the public
  trait. Jellyfin in the POC is direct play by URL; the library browser
  comes right after.
- 2026-11-12: named `gpui-av` (repo `karyalabs/gpui-av`).
- 2026-11-12 (E6): write our own backends; borrow from `gpui-video-player`
  (MIT) where useful. The backend API takes request headers from day one
  (Mattermost file URLs need auth), and colour range and matrix come from the
  decoder, never guessed.
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
