# E7: Matroska and WebM where the platform will not open them

## Question

AVFoundation and browsers decode H.264, HEVC, AAC and (mostly) VP9, AV1 and
Opus, but AVFoundation opens no Matroska or WebM, and Safari opens no
Matroska. How does gpui-av play those files there without bundling a
demuxer or decoder library (rule: platform codecs only), and at what cost?

## Setup

- 2026-11-15, cloud Linux container (4 cores, no GPU), Rust 1.98, ffmpeg
  6.1 and GStreamer 1.24 from Ubuntu 24.04 as references only.
- Options weighed:
  1. **Our own demuxer feeding the decoders** (VideoToolbox on macOS,
     WebCodecs on the web). Most control, but it means writing a second
     player per platform: clock, audio output, A/V sync, seeking. The
     platform players already do all of that well.
  2. **GStreamer on macOS.** Users would have to install it (E6 ruled this
     out as the default).
  3. **Remux to fragmented MP4 and keep the platform player.** Moving coded
     frames between containers decodes nothing, so the platform's (hardware)
     decoders still do the work. AVFoundation takes fMP4 through an HLS
     playlist; browsers through Media Source Extensions (MSE). Chosen.
- `crates/gpui-av-remux`: pure Rust, no dependencies, no `unsafe`, builds
  for wasm32. Reads EBML, the SeekHead, Tracks, Cues and Clusters (all
  three lacings, header stripping), plans segments at keyframes (from the
  Cues, or one scan of cluster headers when there are none), and writes
  `ftyp`/`moov` plus `moof`/`mdat` per segment with `avc1`, `hvc1`, `av01`,
  `vp09`, `mp4a`, `Opus` and `fLaC` sample entries, `colr` and `pasp`.
- Clips: the tier B Matroska and WebM (`scripts/make-tier-b.sh`), plus
  30-second clips with keyframes every 2 s (H.264 with 3 B-frames and AAC,
  H.264 and Opus without Cues, HEVC and Opus at 24 fps, VP9 and Opus), the
  tier B 10-minute and 4K60 MP4 copied into Matroska.
- Run: `GPUI_AV_REMUX_CLIPS=<folder> cargo test -p gpui-av-remux --release
  -- --ignored --nocapture`. It writes `<clip>.remux/` with `index.m3u8`,
  `init.mp4`, `N.m4s` and `whole.mp4`. Checked with ffprobe (packet counts
  and times), `ffmpeg -f framemd5` (decoded frames against the source), each
  segment decoded on its own after the init segment, and GStreamer
  `qtdemux`.
- Step 2: `HlsServer` (in the crate, not on wasm) serves the playlist and
  segments from `127.0.0.1` under a random 128-bit path, and the
  AVFoundation backend plays `.mkv` and `.webm` files through it. Checked
  over HTTP with `cargo run -p gpui-av-remux --example serve -- <clip>`
  and GStreamer `playbin3` (its HLS demuxer takes only http URLs) and
  ffmpeg's HLS demuxer.

## Result

**Remuxing works and costs next to nothing.** On every clip:

- Every video and audio frame comes through: packet counts match the source
  (up to 18,000 video and 25,841 audio frames on the 10-minute clip).
- Decoded video is identical frame for frame to the source's.
- Audio stays in sync with video to the millisecond (the source's offset,
  23 ms of AAC priming or 6.5 ms of Opus pre-skip, is kept).
- Each segment starts on a keyframe and decodes alone after the init
  segment.
- Over HTTP from `HlsServer`, GStreamer `playbin3` plays H.264, HEVC, VP9
  and AV1 clips to the end (the 10-minute one too), and ffmpeg reads every
  frame (18,000 and 25,841 on the 10-minute clip).
- Files without Cues work (one scan of cluster headers at open).
- Cost: 30 MB of Matroska (the 10-minute clip and the 4K60 clip) remux in
  0.08 s in a release build, all segments written, on one core of a cloud
  container. A player asks for one segment at a time, so this is a few
  milliseconds per segment and never on the frame path.

Details worth keeping:

- **Decode times.** Matroska stores presentation times only. Decode times
  are those sorted and moved earlier by the reorder delay seen in the first
  cluster (67 ms for x264's B-frames), so no frame is decoded after it is
  shown; a later group that reorders further is pulled in. Composition
  offsets come from where the sample lengths put each frame, since a
  fragment states only its first decode time.
- **No negative times.** B-frame reordering and audio priming would put
  times before zero, which `tfdt` cannot hold. Every output time is moved
  later by `presentation_offset()` instead (67 ms here). HLS players start
  their timeline at the first frame; an MSE player sets `timestampOffset`
  to minus it.
- **Segments** are at least 3 s, cut at the lead track's keyframes, so a
  2-hour film has a playlist of about 2,400 lines at most.
- **Audio lengths** come from the codec (1024 samples for AAC, the TOC byte
  for Opus) rather than Matroska's millisecond times, which would make each
  frame's length jitter.
- One video and one audio track are taken (the default audio track). Other
  tracks are listed by `skipped()`; picking another audio track and turning
  Matroska subtitles into WebVTT are later work.

Not measured here (needs a Mac and a browser, in the batched Mac check):
AVFoundation playing the HLS playlist from a localhost server (Apple's HLS
takes H.264, HEVC, AAC, FLAC and Opus in fMP4, but not VP9), and Chrome,
Firefox and Safari through MSE. VP9 on macOS stays a gap after this: neither
the HLS path nor AVFoundation's own files take it.

### Step 3: the web, through Media Source Extensions

`gpui-av-web` fetches the file in parts with range requests into a
`SparseFile` (the remuxer reads it; a read of bytes not fetched yet fails
and says where, and the backend fetches them and tries again, doubling the
chunk from 64 KB), then feeds the init segment and each media segment to a
`SourceBuffer` on the same `<video>` element: about 30 s ahead of the
playhead, dropping what is more than 20 s behind, starting again from the
playhead's segment after a seek. It only does this when the element itself
refuses the file (`MEDIA_ERR_SRC_NOT_SUPPORTED`) and its first bytes are
EBML, or always with `WebBackend::remux_matroska_always` (the lab's
`?remux-always`). A codec string `MediaSource.isTypeSupported` refuses
fails as `Unsupported`, so a `Fallback` can move on.

Headless Chromium 153 on Linux (this container, software decoding),
`scripts/web build` release, `scripts/serve.py`, muted:

| Clips | `<video>` itself | Through MSE (`?remux-always`) |
|---|---|---|
| Tier B, 19 clips | 15 pass | 15 pass, the same 15 |
| Tier A, 16 clips | 15 pass | 15 pass (MSE not used: no Matroska) |
| 30 s H.264 B-frames, H.264 without Cues, VP9 and a 10-minute H.264, all Matroska or WebM | 4 pass | 4 pass |

- On the five tier B Matroska and WebM clips, MSE plays the same frames
  (134 to 138 of 150 in the bench's 4.5 s) with 0 or 1 dropped.
- Time to first frame through MSE is 70 to 145 ms against 20 to 150 ms
  natively: the sniff and the header fetch come first.
- Seeks to the middle took 24 to 37 ms on the 30 s clips and 156 ms on
  the 10-minute one (the segment is fetched and remuxed on demand) against
  38 to 72 ms natively.
- HEVC in Matroska: Chromium's `<video>` "plays" it with no picture and
  reports sound only; through MSE it fails as `Unsupported` ("cannot play
  hvc1... from this matroska file"), which lets a fallback take it. The
  other tier B failures are HEVC in MP4, which Chromium has no decoder for
  (Chrome on a Mac does, E3).
- Two things found running it: a `SourceBuffer` refuses a view of the
  page's memory, which GPUI shares with its workers, so each segment is
  copied out first; and Matroska muxers put the sound just before a
  keyframe into the next cluster, so a segment's buffered range stops a
  little short of its end and the feeder must move on to the next segment
  rather than append the same one again.

Not run: Safari and Firefox, where this path matters most (neither opens
Matroska), with the Mac check.

## Next

- macOS: done in code (`HlsServer`, and the AVFoundation backend opens
  `.mkv` and `.webm` files through it; AVAssetResourceLoader does not serve
  HLS media segments, so a real server it is). Waiting on the Mac check:
  tier B on AVFoundation, and whether HLS takes Opus and refuses VP9
  cleanly so the fallback moves on. Matroska over HTTP needs a reader with
  range requests, later.
- Web: done (step 3). Safari and Firefox runs with the Mac check.
- Spec: E7's answer is folded into section 5 and S7.
