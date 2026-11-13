# E2: Linux, GStreamer + RenderImage

## Question

On Linux, GStreamer's `playbin` decodes and GPUI draws each frame as an
ordinary image (`RenderImage`). What does getting a frame from the decoder
into that image cost per frame, at 720p and 1080p, and does the tier A
corpus play correctly this way?

## Setup

- Machine: the cloud sandbox, Ubuntu 24.04, Intel Xeon @ 2.10 GHz, 4 cores,
  no GPU. GPUI draws through Mesa's llvmpipe (Vulkan on the CPU) into Xvfb.
- GStreamer 1.24.2 from Ubuntu: base, good, bad and libav plugins
  (H.264 and HEVC decode in software through gst-libav; no VA-API here).
- GPUI: gpui-pre 0.3.12 with Karya Labs' patches (the workspace default).
- Clips: the tier A corpus from `scripts/make-clips.sh`; three more made
  the same way for format coverage (VP9 in WebM, H.264 in MKV, AV1 in MP4).
- Backend: `crates/gpui-av-gstreamer` as merged with this experiment
  (spec 4.2). Its video branch is
  `videoflip video-direction=auto ! videoconvert n-threads=0 ! appsink`
  with BGRA caps; the appsink's callback copies each frame into a
  `RenderImage` on GStreamer's thread, and `Session::paint` hands the
  newest one to `paint_image`.

How to run:

```sh
sudo apt install gstreamer1.0-plugins-base gstreamer1.0-plugins-good \
  gstreamer1.0-libav gstreamer1.0-tools     # what the backend needs
scripts/make-clips.sh clips
# Correctness and the lab's numbers (in the sandbox: Xvfb :99 first)
cargo run --release -p gpui-av-lab -- --bench corpus/tier-a.json --backend gstreamer
# The same over HTTP with a token (scripts/serve.py clips in another shell)
cargo run --release -p gpui-av-lab -- --bench corpus/tier-a.json --backend gstreamer \
  --base-url http://127.0.0.1:8090/hop/ --header "Authorization: Bearer lab"
# Per-step costs: decoder alone, then with the conversion
experiments/e2-gstreamer/measure.py clips/h264-1280x720.mp4 clips/h264-1920x1080.mp4
# The copy into the image and paint_image, per frame (printed when a
# session closes)
GPUI_AV_GSTREAMER_STATS=1 cargo run --release -p gpui-av-lab -- --bench corpus/tier-a.json --backend gstreamer
```

## Result

**Correctness: yes.** Tier A passes 15 of 15 from files, 15 of 15 over
HTTP with a bearer token, and 15 of 15 through a redirect to another host.
The token reaches every same-host request (range requests and same-host
redirects included) and never the other host: libsoup drops
`Authorization` on a cross-host redirect, as a browser does. Without the
token the server's 401 arrives as `Unauthorized`. A 120 s soak looping the
1080p clip kept memory flat (7 MB growth, 381 to 388 MB). VP9/WebM,
H.264/MKV and AV1/MP4 also play, which AVFoundation and browsers do not all
do (E7). The FFV1 clip, which no platform decoder plays, plays here
because gst-libav decodes it; the manifest now lets it play on
`gstreamer` (`may_play_on`).

**Cost of the copies: about as much CPU as decoding.** CPU milliseconds per
frame, best of three runs (`measure.py`), and the backend's own counters
from the bench:

| Clip | Decode (software) | + convert to BGRA | Copy into `RenderImage` | `paint_image` (GPUI's upload, CPU side) | Copies in total |
|---|---|---|---|---|---|
| H.264 1280x720 30 fps | 2.98 | +0.35 | 0.93 to 1.1 | 0.51 to 0.60 | about 1.9 ms |
| H.264 1920x1080 30 fps | 5.66 | +1.16 | 2.3 to 2.7 | 1.2 to 1.3 | about 4.9 ms |
| H.264 1920x1080 60 fps | 4.76 | +1.22 | 3.2 to 3.3 | 1.55 | about 6.0 ms |
| HEVC 1920x1080 30 fps | 7.93 | +1.55 | 2.3 | 1.21 | about 5.1 ms |

At 1080p60 the three copies cost about 6 ms of CPU per frame, a third of
one core, before any decoding. With a hardware decoder (VA-API) the decode
column goes to nearly zero but the copies stay, and grow by one: the frame
must first be read back from video memory. At 4K they are four times
larger. Conversion on every core (`n-threads=0`) cuts the wall time of the
conversion, not its CPU. The copy into the image is the most expensive
step because each frame is a new 8 MB allocation (GPUI's `RenderImage`
owns its pixels and gives no way to reuse them), so the kernel maps fresh
pages every frame.

**Smoothness: not measurable here.** The sandbox's window draws about
21 frames a second (llvmpipe renders GPUI's whole scene on the CPU), so
frames shown, frames dropped, drift (about 65 ms, one window frame) and
process CPU in the bench measure the sandbox, not the backend. Time to
first frame was 85 to 180 ms and seeks 80 to 380 ms. Real numbers need a
Linux desktop with a GPU.

## A Linux VM on a Mac (2026-11-13)

Tried as a stand-in for a Linux desktop with a GPU: Lima 2.2.1 with
krunkit 1.3.2 on an M4 Max (48 GB), Fedora 44 arm64, 8 cores, 16 GiB,
GStreamer 1.28.9, full ffmpeg from RPM Fusion, and libkrun's Mesa
25.3.6 from the `slp/mesa-libkrun-vulkan` COPR (Fedora's own Mesa gives no
Venus device at all).

- **GPUI cannot open a window there.** The Venus device ("Virtio-GPU
  Venus (Apple M4 Max)", Vulkan 1.2) has no `VK_KHR_swapchain` and no
  dma-buf export under krunkit, so wgpu picks it and panics creating the
  swapchain, under Weston (headless) and Xvfb alike. Forcing llvmpipe
  crashes inside LLVM 22's JIT. So the VM gives no window numbers.
- **Decode and convert numbers do work** (`measure.py`, CPU ms per frame):

| Clip | Decode | + convert (one thread) |
|---|---|---|
| H.264 1280x720 30 fps | 1.81 | +1.03 |
| H.264 1920x1080 30 fps | 3.65 | +2.35 |
| H.264 1920x1080 60 fps | 2.96 | +2.61 |
| HEVC 1920x1080 30 fps | 4.54 | +2.50 |

  Decoding is about 35 to 40% cheaper per frame than on the sandbox's
  Xeon; the conversion to BGRA costs about twice as much (arm64, where
  GStreamer's converter has less hand-tuned code). Either way the
  conversion alone is half to two thirds of the decode cost.
- A wgpu context that falls back to an adapter that can present (instead
  of panicking on the first adapter) would make GPUI start on machines
  like this one; a candidate patch for our GPUI.

## Next

- The RenderImage path is correct and is the Linux default, but its
  copies cost as much as decoding. Zero-copy on wgpu (E5, our GPUI's patch
  4: draw dmabuf frames from VA-API with no copy) is where the cost goes
  away, and moves up in priority.
- Cheaper on the way there: convert YUV to BGRA straight into the image's
  buffer (one pass instead of two copies), and a GPUI change that lets an
  image's buffer be reused (no fresh pages per frame).
- Run the bench and a 1080p60 soak on a Linux desktop with a GPU, on X11
  and Wayland, with VA-API decode.
- GStreamer fills in colour for streams that state it and leaves it empty
  otherwise (seen on the untagged VP9, MKV and AV1 clips); check with
  tier B whether any decoder fills in a guess.
