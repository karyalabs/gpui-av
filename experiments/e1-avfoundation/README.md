# E1: AVFoundation and GPUI's surface on macOS

## Question

Does `AVPlayer` + `AVPlayerItemVideoOutput`, drawn with GPUI's `surface`,
play 1080p60 smoothly inside a GPUI window, with sound, behind the public
`gpui_av::Backend` trait?

## Setup

- 2026-11-13, MacBook Pro, Apple M4 Max, 48 GB, macOS 27.2 (26B123),
  built-in 120 Hz display. Rust 1.99.1, gpui-pre 0.3.12,
  objc2-av-foundation 0.3.2, objc2-core-video 0.3.2 (the version
  gpui-pre uses, so `CVPixelBuffer` is the same type).
- The code is the real backend, not a throwaway: `crates/gpui-av-avfoundation`.
- Clips from `scripts/make-clips.sh` (ffmpeg 8 `testsrc2` + `sine`, H.264
  High, AAC, BT.709 limited range, faststart). The smoothness clip is
  `h264-1920x1080-60fps.mp4`, 60 s, 72 MB.
- Run with the example, release build:

  ```sh
  scripts/make-clips.sh clips
  cargo build --release -p gpui-av-avfoundation --example play
  /usr/bin/time -l target/release/examples/play --quit-after 61 clips/h264-1920x1080-60fps.mp4
  target/release/examples/play --seek-test clips/h264-1920x1080-60fps.mp4
  target/release/examples/play --header "Authorization: Bearer test-token" http://127.0.0.1:8765/ok/clip.mp4
  ```

  Add `--muted` for long runs. The example logs state changes, media info, position, frames presented
  and dropped, and window frames per second, and with `--quit-after`
  prints a summary. CPU and peak memory come from `/usr/bin/time -l`,
  memory over time from `ps` every 5 s.
- HTTP: a local Python server (not committed) that serves the clips with
  range requests, answers 401 without `Authorization: Bearer test-token`,
  403 and 404 on other paths, and logs every request's headers. For the
  seek test it was throttled to 4 MB/s so seeking had to open new ranges.
- "Dropped" is counted by the session: when the frame it takes from the
  output is more than one frame duration (the track's `minFrameDuration`)
  after the previous one while playing, the frames between count as
  dropped. Seeks and loops do not count.

## Result

**Yes.** 1080p60 plays with sound, every frame shown, at about a sixth of
one core.

| 1080p60 H.264, 60 s, local file | |
|---|---|
| Frames presented / dropped | 3600 / 0 (0.00%) |
| Frames per second while playing | 59.2 (the 60 s includes the start) |
| Window frames per second | 120 (GPUI redraws at the display rate) |
| CPU | 11.5 s over 61 s (5.9 user + 5.7 sys): ~19% of one core; `ps` showed 16 to 19% |
| Memory | RSS ~109 MB, flat through the run; peak footprint 375 MB (includes IOSurfaces) |
| Time to first frame | 195 to 240 ms from creating the player (local file, cold process) |
| Seek to the middle (local, paused frame ready) | 85 ms |
| Sound | coreaudiod holds an `audio-out BuiltInSpeakerDevice` assertion for the player's PID |

Loop (`--loop`, same clip): stopped after about 3 minutes rather than 10
(the test tone was playing aloud). RSS sampled every 30 s went
107.7, 107.6, 107.7, 107.8, 108.1, 108.4, 108.8 MB, crossing two loops: no
sign of a leak, but the 10 minute check (POC criterion 3) is still to run,
muted, in the lab's bench.

Correctness (all through the example):

| Case | Result |
|---|---|
| 1280x720, 1920x1080 30 fps | Plays; `MediaInfo` 20 s, size, limited range, BT.709 |
| Rotation tag 90° (`h264-1280x720-rotate90`) | Shows portrait; `natural_size` 720x1280, `Cw90`; TTFF 300 ms |
| moov at the end, no colour tags | Plays; range `Limited`, matrix `None` (not in the file, so not guessed) |
| HTTP with `Authorization` and `X-Test` | Plays; all 28 requests carried both headers, ranges after a seek included |
| HTTP seek at 4 MB/s | `Buffering` then `Playing`; settled in 1.6 s (bandwidth-bound) |
| No auth (401) | `Failed(Unauthorized)` |
| 403 | `Failed(Unauthorized)`, see note 3 |
| 302 to another host, then 403 | `Failed(Unauthorized)`; see note 5 on the headers |
| 404 | `Failed(NotFound)` |
| Server down | `Failed(Network)` |
| Missing file | `Failed(NotFound)` at once, from `open` |
| Truncated file | Plays to the cut, then `Failed(Decode)` |
| Random bytes as `.mp4` | `Failed(Decode)` |
| MKV | `Failed(Unsupported)` (the E7 gap) |
| FFV1 in MOV (no decoder) | `Failed(Unsupported)`: "no decoder for the FFV1 track" |

### What it took

1. **GPUI's surface takes one pixel format and one matrix.** The Metal
   renderer in gpui-pre-apple 0.3.12 refuses anything but
   `kCVPixelFormatType_420YpCbCr8BiPlanarFullRange` and converts with a
   hard-coded full-range BT.601 matrix. Most video is limited-range BT.709.
   The output therefore asks AVFoundation for `420f` *and* for BT.601 colour
   properties (`AVVideoColorPropertiesKey`): the frames come out as
   `420f`, `ITU_R_601_4`, `ITU_R_709_2` transfer (checked on the buffers'
   attachments), so colours are right, at the cost of a conversion pass
   VideoToolbox does on the GPU. `MediaInfo.color` still reports what the
   file says (limited, BT.709), read from the track's format description.
   A zero-copy path for limited-range or BT.709 frames needs GPUI's shader
   to read the buffer's attachments: an upstream change (risk R2).
2. **Rotation.** `paint_surface` cannot rotate. For a track with a
   rotation tag the session sets an `AVVideoComposition` built from the
   asset, which applies the track's transform. Unrotated video has no
   composition.
3. **HTTP status.** A 401 surfaces as `NSURLErrorDomain -1013`, a 404 as
   `-1100`, but a 403 comes back as "Cannot Open, this media may be
   damaged" (`AVFoundationErrorDomain -11829`, `OSStatus -12848`) with no
   status anywhere, and `AVPlayerItem.errorLog` is empty for progressive
   download. When an HTTP source fails with a vague error, the session asks
   the server for two bytes with the same headers and reports by that
   status. The probe refuses redirects, so it never sends the headers
   anywhere but the URL the app gave. (Behind a redirect, the same 403
   arrives as `NSURLErrorDomain -1102` / `OSStatus -12660`, which map
   directly.)
4. **Undecodable tracks play as nothing.** AVPlayer "plays" an FFV1 file to
   the end without a picture or an error. The session checks
   `AVAssetTrack.isDecodable` when the item is ready and fails with
   `Unsupported`.
5. **Request headers use an unexported-in-headers key.**
   `AVURLAssetHTTPHeaderFieldsKey` is exported by AVFoundation (it is in
   the SDK's `.tbd`) but declared in no public header, so it is linked by
   hand. It works and every request carries the headers, but App Store
   review has rejected it in the past. **It also follows redirects with
   the headers:** on a 302 from `127.0.0.1` to `localhost`, AVFoundation's
   first request to the new host had no `Authorization`, but its retry did.
   So a server that redirects to another host (a CDN) gets the app's
   credentials. The documented alternative, an
   `AVAssetResourceLoaderDelegate` with a custom scheme doing the HTTP
   ourselves, fixes both and lets the backend decide per redirect. It is
   the first follow-up for this backend.
6. **Build.** Xcode 26+ ships without the Metal compiler, and gpui-pre-apple
   compiles its shaders in `build.rs`. The example uses gpui-pre-platform's
   `runtime_shaders` feature, so nothing in this repo needs the Metal
   toolchain. Apps that build GPUI's shaders ahead of time need
   `xcodebuild -downloadComponent MetalToolchain`.

### The trait

- **Added `Session::stats()`** (frames presented and dropped, default
  zero) and `VideoPlayer::stats()`. Nothing else could report drops, and
  the lab's overlay and bench need them.
- **Stats vanish on failure.** The player drops its session when it
  fails, so `stats()` reads zero afterwards (a truncated file showed 143
  frames presented, then 0). The player should keep the last stats.
- **A failure found at `open` emits no event**, since the player starts in
  `Failed`. Fine for the API, but every app (the example included) has to
  check `state()` right after creating the player as well as subscribing.
- **No frame timing from GPUI.** The session picks the frame for "now"
  from the host clock during paint; the frame is shown at the next vsync.
  At 120 Hz that is at most 8 ms early. A display-link timestamp in
  `Placement` or `Window` would let a session aim at the vsync.
- **State through polling.** The session polls the player every 20 ms on
  GPUI's foreground executor (plus notifications for the end and for
  failures) instead of key-value observing, which needs an Objective-C
  subclass. State changes can be up to 20 ms late; it costs nothing
  measurable.

### Not settled

- **One run at 30 fps.** The first 1080p60 run of the session showed 30
  frames presented a second and half the frames dropped, from the start.
  It did not happen again in nine later runs, foreground or background,
  including the 60 s and 10 minute runs. The window frame counter was
  added after that run, so it is not known whether GPUI was painting at
  30 Hz then. Watch for it in the lab.
- **A/V drift** is not measured: the example has no way to read the audio
  clock separately. AVPlayer keeps audio and video on one timebase, and
  frames are picked from that timebase, so drift should be zero, but the
  lab should measure it.
- **Pixel aspect ratio** is not applied to `natural_size`; no tier A clip
  has non-square pixels.
- Audio was confirmed by the system's audio assertion for the process, not
  by ear.

## Next

- Spec: E1 answered yes; the AVFoundation backend is the macOS backend of
  the POC. Decisions recorded in section 7.
- The lab (L0) takes this backend as is and measures drift, seek and
  memory through `--bench`.
- Upstream candidates for GPUI: let `paint_surface` take limited-range and
  BT.709 NV12 (read the buffer's attachments), which would remove the
  conversion pass.
- Keep the player's last stats after a failure (gpui-av).
