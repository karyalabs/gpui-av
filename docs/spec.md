# gpui-av: spec

Status: experiment. Nothing here is a promise yet. This file is the plan of
record; change it in the same pull request as the work that changes the plan.

`gpui-av` is a community crate from Karya Labs, not part of Zed's GPUI.
The name says what it is: audio and video for GPUI. (Zed's own `gpui-media`
crate is unrelated macOS bindings.)

## 0. Start here

**Where things stand (2026-11-13).** The proof of concept (section 5.2) has
started: the workspace, the `gpui-av` API crate and the macOS backend
(`gpui-av-avfoundation`) exist (section 4.2). E1 is answered: AVPlayer
drawn with `surface` plays 1080p60 with no dropped frames at about a fifth
of a core. E6 is done (section 5): we write our own backends and
borrow ideas from `gpui-video-player`. Everything else is open. When gpui-av works, its player
moves into gpuicn (Karya Labs' GPUI component library) as a component, and
Matty uses it from there. Until then this repo stands alone: no other Karya
Labs repo depends on it, and nothing here needs Matty or gpuicn to build.

The first thing to build is the proof of concept in section 5.2: two
platforms (macOS and the web), the real crate, the lab, and numbers.

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
| E2 | Linux; a desktop with a GPU gives real numbers, a cloud container only gives CPU numbers |
| E3, E4 | A browser; Chrome works in a cloud container, Firefox and Safari need a real machine |
| E5 | Linux with a Vulkan GPU; it patches gpui-pre-wgpu locally |
| E6 | Done |
| E7 | A Mac and Chrome; MKV clips from the corpus |
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
  decode, audio output and the clock, and reports state. Backends are
  pluggable: each platform has a default, and an app can pick another one
  (or bring its own) through the public `Backend` trait, without platform
  types or `unsafe` in that trait. Backends:
  - macOS: `AVPlayer` + `AVPlayerItemVideoOutput`, giving `CVPixelBuffer`s.
  - Linux: GStreamer `playbin` with an `appsink`, loaded at run time so the
    app still starts without GStreamer; without it, fall back to opening the
    file in the system player.
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
`info` (and `buffered`, after the POC). Events: `StateChanged`,
`InfoChanged`, `Seeked`. Controls (play button, timeline, volume, fullscreen) live in gpuicn,
not here, so gpui-av stays a media crate.

### 4.2 How the crates fit (as built)

```
crates/gpui-av                the API; depends on the GPUI core crate only
crates/gpui-av-avfoundation   macOS backend (E1), depends on gpui-av
crates/gpui-av-web            web backend (E3), depends on gpui-av
lab/                          gpui-av-lab (L0), with its test-pattern backend
```

- **Backends depend on the API crate, not the other way round.** `gpui-av`
  knows no backend. An app picks one at startup with
  `gpui_av::set_default_backend`, or per player with
  `VideoPlayer::with_backend`; a player created with no backend set starts
  in `Failed`. A facade crate that picks the platform's default for the app
  can come later without changing this.
- **`Backend` is a factory, `Session` is one piece of media.**
  `Backend::open(source, events, cx)` returns a `Session`, which takes the
  commands (`play`, `pause`, `seek`, `set_volume`, `set_muted`,
  `set_looping`), answers `position`, and reports everything else (media
  info, state changes, finished seeks) through an `EventSink` that works
  from any thread. The backend is the source of truth for
  `PlaybackState`; the player mirrors it.
- **The presenter is `Session::paint`.** The `video()` element works out
  where the picture goes (object fit, from the reported natural size) and
  hands the session a `Placement` (the picture's bounds and the clip) plus
  GPUI's `Window`. The session presents however its platform does it best:
  `paint_surface` on macOS, `paint_image` for CPU frames, moving a DOM
  `<video>` on the web. That keeps `CVPixelBuffer` and browser types out of
  the trait, and it is why a backend written outside the crate can draw
  frames at all. While the state is `Playing` or `Buffering` the element
  asks for a new window frame after every paint; the session decides which
  decoded frame is due.
- **Still to settle in E3:** how a session learns that its element left the
  screen while the player lives on (the web overlay must be hidden then).
  A session is dropped with its player, which covers the player going away.
- **Stats.** `Session::stats` reports frames presented and dropped (zero
  for a session that cannot tell); `VideoPlayer::stats` passes them on for
  the lab's overlay and bench.
- **macOS (E1).** GPUI's surface takes only full-range NV12 and converts
  with a fixed BT.601 matrix, so the AVFoundation backend asks
  `AVPlayerItemVideoOutput` to convert each frame to exactly that
  (`420f`, BT.601 colour properties). Rotated tracks go through an
  `AVVideoComposition`, since `paint_surface` cannot rotate. Request
  headers use `AVURLAssetHTTPHeaderFieldsKey` for now (section 6, R6).
- **Media info** carries duration, natural size (as shown, rotation
  applied), the rotation tag, whether there is video and audio, and colour
  range and matrix as optional values: a backend leaves out what the
  decoder did not report.

## 5. Experiments

Each experiment is a folder under `experiments/` with a README holding the
question, the setup, the result and the numbers. An experiment answers one
question; when it is done, the answer is folded into this spec.

| ID | Question | Done when |
|---|---|---|
| E0 | What do real Mattermost videos look like (containers, codecs, sizes, lengths)? | A table from a sample of real attachments |
| E1 | macOS: does AVPlayer + `surface()` play 1080p60 smoothly inside a GPUI window? | A demo plays with sound; CPU and dropped frames measured. **Answered 2026-11-13: yes**, 0 of 3600 frames dropped at ~19% of one core; marked done with the POC. See `experiments/e1-avfoundation` |
| E2 | Linux: GStreamer + RenderImage at 1080p: what does the per-frame copy cost? | CPU, memory and dropped frames at 720p and 1080p |
| E3 | Web: a DOM `<video>` overlay that tracks a GPUI element through scroll, resize and close | Works in Chrome, Firefox and Safari |
| E4 | Web: WebCodecs frames drawn inside the canvas without an overlay | One clip plays in Chrome; cost measured against E3 |
| E5 | Zero-copy on wgpu: implement `PrimitiveBatch::Surfaces` (NV12) in gpui-pre-wgpu | Patch works locally; decide whether to propose it to Zed |
| E6 | Can we build on gpui-video-player or alhazen instead of writing backends? | **Done 2026-11-12: no; reuse ideas.** See `experiments/e6-existing-crates` |
| E7 | Containers the platform will not open (MKV on AVFoundation and in browsers): our own Rust demuxer feeding VideoToolbox / WebCodecs (Mediabunny on the web), or an optional GStreamer backend on macOS? | The MKV clips in the corpus play on macOS and in Chrome through the chosen path; cost measured |
| L0 | Build `gpui-av-lab`, the playground and benchmark app (section 5.1) | Interactive mode and `--bench` play the tier A corpus from local and HTTP sources |

Order: E0 and E6 first (cheap, and they can cancel work), then E1 and E3
(the two biggest user groups after the web), then E2, E4 and E5. E7 follows E1
and E4, since it reuses their decode paths. L0 starts
with E1: its first backend is the E1 code, and every later experiment
reports its numbers through `--bench`.

E1, E3 and that first slice of L0 together make the proof of concept
(section 5.2), which comes before everything else that is still open.

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
are listed by URL and SHA-256 and fetched by a script. Arya hosts the same
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

Status: agreed 2026-11-12. In progress: the workspace, the API crate and
the macOS backend are in (section 4.2); the web backend and the lab are
next. First macOS numbers, from E1's example rather than the bench
(M4 Max, 1080p60 H.264, local file): 0 of 3600 frames dropped in 60 s,
time to first frame 195 to 240 ms, a seek settles in 85 ms; A/V drift not
yet measured.

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
   size, rotation and duration match the manifest on both; colour range
   and matrix match on macOS, and are reported as unknown on the web,
   where `<video>` does not expose them.
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
    `wasm32-unknown-unknown`, against a pinned gpui-pre snapshot.

When the POC is done, E1 and E3 are marked done, L0 is marked done for
tier A, and the next milestone is Linux (E2).

## 6. Risks

- **R1. GPUI churn.** gpui-pre ships a new snapshot about weekly, and the
  existing video crates broke on exactly this. Keep the surface we touch
  small, and run a canary against new snapshots.
- **R2. Renderer ownership.** Zero-copy outside macOS needs renderer changes
  we do not own. Treat it as optional; nothing in goals 1 to 4 depends on it.
- **R3. Small audience.** About 100 to 150 crates depend on GPUI. The aim is
  to serve all of them, not only Matty, so the API and backends are judged
  against general use; Matty is the first proof, not the bar.
- **R4. Codecs and law.** Use only the decoders the OS or browser already
  has; never bundle GStreamer plugins or ffmpeg. Not legal advice.
- **R5. Scope.** One maintainer plus Claude. Keep to the non-goals.
- **R6. Request headers on macOS.** `AVURLAssetHTTPHeaderFieldsKey` is
  exported but not in a public header (App Store review has rejected it),
  and AVFoundation sends the headers on to the target of a cross-host
  redirect. An `AVAssetResourceLoaderDelegate` doing the HTTP ourselves
  removes both; it is the next step for the macOS backend (E1).

## 7. Decisions

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
