# E3: the web overlay

## Question

Can a DOM `<video>` element, kept over a GPUI element's bounds on top of
GPUI's canvas, play the tier A corpus in Chrome behind the same public
API as the macOS backend, and stay with its element through scroll,
resize and close?

## Setup

- 2026-11-12, MacBook Pro, Apple M4 Max, 48 GB, macOS 27.2, built-in
  120 Hz Retina display (plus a 2560x1440 external display).
- Google Chrome 157.0.8103.51 (the app bundle reads 156.0.8078.80; a
  pending update was running), in a throwaway profile
  (`--user-data-dir` in a temporary folder), muted. Safari 27.2.
  Firefox is not installed.
- `gpui-av-web` and `gpui-av-lab` built for `wasm32-unknown-unknown`
  with `scripts/web build` (nightly-2026-08-17, wasm-bindgen 0.2.130,
  release), gpui-pre 0.3.12.
- Clips: the tier A corpus from `scripts/make-clips.sh`, checked against
  `corpus/tier-a.json`.
- Served by `scripts/serve.py --log`: the lab at `/`, the clips at
  `/clips/` and again at `/private/` behind `Authorization: Bearer lab`.

```sh
scripts/web serve clips
# then, in Chrome:
http://127.0.0.1:8090/?bench=tier-a.json&base-url=clips/&out=reports/bench-web.json
http://127.0.0.1:8090/?bench=tier-a.json&base-url=private/&header=Authorization:%20Bearer%20lab&out=reports/bench-web-auth.json
http://127.0.0.1:8090/?bench=tier-a.json&base-url=private/&out=reports/bench-web-noauth.json
http://127.0.0.1:8090/?soak=600&clip=clips/h264-1920x1080-60fps.mp4&out=reports/soak-web.json
```

## Result

**In Chrome, yes:** every tier A clip plays, H.264 and HEVC, MP4 and MOV;
the broken clips fail with the right kind; a source with an
`Authorization` header plays through the service worker with range
requests, and fails `Unauthorized` without it.

### Chrome bench

Frame counts are Chrome's own (`getVideoPlaybackQuality`); a browser
does not report drift. "First frame" is from creating the player to the
first frame Chrome counts, which is earlier than a frame on screen would
be on macOS (there the lab sees the surface painted).

**Same origin, `/clips/`:** 15 of 15 pass.

| Clip | Outcome | First frame ms | Seek ms | Video fps | Dropped % | Size | Tracks |
|---|---|---|---|---|---|---|---|
| h264-1280x720.mp4 | played | 17 | 16 | 30.0 | 0.0 | 1280x720 | video + audio |
| h264-1920x1080.mp4 | played | 20 | 24 | 30.0 | 0.0 | 1920x1080 | video + audio |
| h264-1920x1080-60fps.mp4 | played | 18 | 30 | 60.0 | 0.0 | 1920x1080 | video + audio |
| h264-720x1280-portrait.mp4 | played | 11 | 24 | 29.9 | 0.0 | 720x1280 | video + audio |
| h264-1280x720-rotate90.mp4 | played | 10 | 22 | 29.9 | 0.0 | 720x1280 | video + audio |
| h264-1280x720.mov | played | 19 | 18 | 30.0 | 0.0 | 1280x720 | video + audio |
| hevc-1920x1080.mp4 | played | 19 | 33 | 30.0 | 0.0 | 1920x1080 | video + audio |
| h264-1280x720-vfr.mp4 | played | 18 | 14 | 19.0 | 4.0 | 1280x720 | video + audio |
| h264-1280x720-noaudio.mp4 | played | 9 | 22 | 30.2 | 0.0 | 1280x720 | video |
| aac-audio-only.m4a | played | – | – | – | – | – | audio |
| h264-1280x720-moov-at-end.mp4 | played | 27 | 22 | 30.0 | 0.0 | 1280x720 | video + audio |
| broken-truncated.mp4 | failed, Decode | 10 | – | 27.3 | 0.0 | 1280x720 | – |
| broken-not-media.mp4 | failed, Unsupported | – | – | – | – | – | – |
| unsupported-ffv1.mov | failed, Unsupported | – | – | – | – | – | – |
| missing.mp4 | failed, NotFound | – | – | – | – | – | – |

**With `Authorization: Bearer lab`, `/private/`, through the service worker:** 15 of 15 pass.

| Clip | Outcome | First frame ms | Seek ms | Video fps | Dropped % | Size | Tracks |
|---|---|---|---|---|---|---|---|
| h264-1280x720.mp4 | played | 26 | 17 | 30.0 | 0.0 | 1280x720 | video + audio |
| h264-1920x1080.mp4 | played | 18 | 25 | 29.8 | 0.0 | 1920x1080 | video + audio |
| h264-1920x1080-60fps.mp4 | played | 19 | 31 | 60.0 | 0.0 | 1920x1080 | video + audio |
| h264-720x1280-portrait.mp4 | played | 9 | 20 | 30.4 | 0.0 | 720x1280 | video + audio |
| h264-1280x720-rotate90.mp4 | played | 9 | 24 | 30.2 | 0.0 | 720x1280 | video + audio |
| h264-1280x720.mov | played | 9 | 26 | 30.2 | 0.0 | 1280x720 | video + audio |
| hevc-1920x1080.mp4 | played | 19 | 35 | 30.0 | 0.0 | 1920x1080 | video + audio |
| h264-1280x720-vfr.mp4 | played | 19 | 20 | 19.2 | 3.0 | 1280x720 | video + audio |
| h264-1280x720-noaudio.mp4 | played | 9 | 26 | 30.6 | 0.0 | 1280x720 | video |
| aac-audio-only.m4a | played | – | – | – | – | – | audio |
| h264-1280x720-moov-at-end.mp4 | played | 18 | 27 | 30.1 | 0.0 | 1280x720 | video + audio |
| broken-truncated.mp4 | failed, Decode | 9 | – | 27.3 | 0.0 | 1280x720 | – |
| broken-not-media.mp4 | failed, Unsupported | – | – | – | – | – | – |
| unsupported-ffv1.mov | failed, Unsupported | – | – | – | – | – | – |
| missing.mp4 | failed, NotFound | – | – | – | – | – | – |

Over `/private/`, all 23 requests reached the server with the token and a
`Range` header (the service worker forwards both). In the auth run the
lab's canvas drew at 30 frames a second after the first clip, while the
video itself kept 60 (1080p60, 0 dropped): the browser composites the
`<video>` element itself, so the canvas's rate does not limit the
picture. Without the header, every clip, the broken ones included, ended
in `Failed(Unauthorized)`.

The variable frame rate clip shows 3 to 4% dropped in every run: Chrome's
own counter, on a clip whose gaps are in the file. Not explained; it
does not affect the constant rate clips.

### The later runs: a virtual screen

The Mac was in use, so the soak and the hand checks ran on a virtual
1920x1080 HiDPI screen at 60 Hz (`scripts/virtual-display`), in a Chrome
window that was never activated (`open -g`, driven through the DevTools
protocol on a local port: navigation, clicks, the wheel, window bounds,
screenshots). The first attempt put the virtual screen in a mirror set
with the built-in display and made it main; it was removed within a
minute. The second un-mirrored it at once, for the session only, and the
real screens kept their origins, modes and the main display throughout
(checked before, during and after).

Validated against the real screen with the same bench: the page was
`visible`, `requestAnimationFrame` ran at 61 to 70 a second, all 15 clips
passed, and 1080p60 showed 59.99 fps with 0 dropped. But GPUI's canvas
drew only 25 to 27 frames a second there (120 on the focused real
screen, 30 on the real screen unfocused): the window never has focus.
Two clips counted 1.3 and 2.7% dropped that did not on the real screen.
So frame counts from the virtual screen are not published as results;
it was used for memory and for looking.

### Soak

10 minutes of the 1080p60 clip, looping, on the virtual screen, sampled
with `ps` once a minute. The page's renderer process: 245, 242, 253, 253,
253, 253, 253, 254, 254, 254, 247 MB (flat after the third minute); the
GPU process 110 to 112 MB. The report: 35,959 frames shown, 13 dropped
by Chrome's count, still `Playing` at the end. An earlier run on the real
screen lost its window at 8.5 minutes (closed by hand); until then the
renderer was at 213 to 248 MB with no steady climb.

### Hand checks (virtual screen, through the DevTools protocol)

| Check | Seen |
|---|---|
| The picture sits in the stage | At 1600x813, 1100x613 and 1880x953 CSS px the `<video>` box was the stage's width, letterboxed to 16:9 inside it (for example x 280 to 1100, y 52.6 to 513.9 at 1100x613); screenshots show no gap or overlap |
| Window resizes | The box followed each resize at once |
| Scroll | The wheel over the video reached GPUI (pointer events pass through); the picture moved with the text (y 240 to -10) and got `clip-path: inset(10px 0 0)`: cut at the stage's top edge |
| Hide | `display: none`; restored when shown again |
| Backend switch | test-pattern: no `<video>` in the page; web again: one, playing |
| Play, Pause, -5 s | Each click reached GPUI and the element (paused, playing, `currentTime` 0) |
| Autoplay without `?muted` | The clip stayed paused at 0.0 until a click on Play, then played (the tab was muted with `--mute-audio`, so nothing was heard) |
| Pixel ratio change | Not verified. With `Emulation.setDeviceMetricsOverride` (2 to 1 and 1.5) GPUI's canvas kept its backing size and drew at the new scale while the overlay stayed where the old layout put it, so they no longer lined up. Emulation may not send the events a real move between screens does; the real test (a 2x and a 1x screen) is still to do |

### What it took

- **A file cut short.** Chrome plays a file cut off inside its media data
  to the cut, then jumps to the stated end and fires `ended`, with no
  error (checked on a plain `<video>`: buffered 0 to 20 s, `ended` after
  about 1 s of wall time). The session now notes where playback was and
  when, and an `ended` that arrives over a second of media before
  playing at normal speed could have reached the end is
  `Failed(Decode)`. A normal end still reports `Ended` (checked with the
  10 s clip played to its end).
- **Audio track.** Chrome has no `audioTracks`; the backend reads
  `webkitAudioDecodedByteCount`, which counts while muted: the no-audio
  clip reports video only, the others video and audio.
- **Rotation.** Chrome applies the rotation tag and reports the size as
  shown (720x1280 for the rotated clip) without the tag itself, so
  `rotation` is unknown on the web.

### Not measured

- **Safari**: not run. It could not be opened without disturbing the
  Safari windows of the person using the Mac. A listed gap.
- **Firefox**: not installed on this Mac.
- **A real pixel ratio change** (above).
- **The Sintel trailer** (`scripts/fetch-real-clips.sh`): left for Arya to
  fetch and judge by eye and ear.

### Limits

- GPUI cannot draw over the video: the element sits above the canvas.
- Full screen is not handled.
- Colour range and matrix are unknown in a browser.

## Next

- Spec: E3's Chrome half measured, apart from a real pixel ratio change;
  Safari and Firefox still to run on a Mac where that is possible.
- Check a move between a 2x and a 1x screen by hand; if the overlay then
  misaligns as under emulation, the web backend must follow the canvas's
  pixel ratio.
