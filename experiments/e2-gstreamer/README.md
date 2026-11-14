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
21 frames a second, so frames shown, frames dropped, drift (about 65 ms,
one window frame) and process CPU in the bench measure the sandbox, not
the backend. Time to first frame was 85 to 180 ms and seeks 80 to 380 ms.
(Corrected 2026-11-14: the cap was GPUI's inactive-window throttle, not
llvmpipe; see the next section. With it off the sandbox draws about
59 frames a second.)

## On a real GPU: OVH A10 (2026-11-14)

Machine: OVH Public Cloud `a10-45` in GRA11: NVIDIA A10 (24 GB, driver
580.196.07), 30 vCPU of an AMD EPYC 9554, 40 GB, Ubuntu 24.04 with its
GStreamer 1.24. Xorg runs on the A10 with no window manager and a virtual
2560x1440 screen; audio has no device (the bench is muted). The harness
and every script are in `ovh/` (see its section below). Spend: ₹330 of
the ₹12,000 budget, three billed hours.

**GPUI on the A10: works, stock and with karyalabs/zed#5 alike.** wgpu
picks the A10 (Vulkan) under Xorg and the window draws correctly. Under
Xvfb it also picks the A10 and presents through a CPU copy, so even an X
server the NVIDIA driver cannot drive gets a GPU-rendered window; zed#5
(skip adapters that cannot present) changes nothing on this machine,
since every adapter here can present. Under Weston's headless backend GPUI
panics at start: "Wayland compositor does not provide wl_seat" (a GPUI
bug for seatless compositors, not a GPU one).

**The 30 fps cap was GPUI's, not the GPU's.** The first runs drew exactly
30 frames a second whatever the present mode (FIFO, FIFO relaxed,
immediate) or the screen's refresh rate (85 or 60 Hz), so every 60 fps
clip dropped half its frames. GPUI holds a window it thinks is inactive to
`WindowOptions::inactive_frame_interval`, 33 ms by default, and on X11
without a window manager no window is ever active. That throttle was also
the sandbox's 21 fps. The lab now sets it to `None`; every app that shows
video needs the same, since video playing in a window without focus is
ordinary. The numbers below are with the throttle off.

**Decoders.** GStreamer picks NVDEC by itself for every clip it has a
decoder for: `nvh264dec`, `nvh265dec` (10-bit too), `nvav1dec` and
`nvvp9dec`. FFV1 stays on gst-libav. The NVIDIA VA-API driver
(`nvidia-vaapi-driver`) also loads, but GStreamer ranks `nv*dec` first.

**Result: everything plays, and NV12 into GPUI cuts the CPU by half to
three quarters.** Tier A passes 15 of 15 and the five heavier clips
(`make-gpu-clips.sh`: 4K H.264, 4K HEVC 10-bit 60 fps, 4K AV1 10-bit,
1080p60 AV1 and VP9) 5 of 5, on every path below. Process CPU in percent
of one core, and video frames dropped, through the image path (convert to
BGRA, copy into a `RenderImage`, GPUI uploads it) and through NV12 drawn
as a surface by our GPUI (E5, karyalabs/zed#7; `--features
karya-gpui-linux`):

| Clip | NVDEC, image | NVDEC, NV12 | Software, image | Software, NV12 |
|---|---|---|---|---|
| H.264 1280x720 30 fps | 70%, 0 | 43%, 0 | 83%, 0 | 58%, 0 |
| H.264 1920x1080 30 fps | 95%, 0 | 32%, 0 | 129%, 0 | 112%, 0 |
| H.264 1920x1080 60 fps | 158%, 0 | 52%, 0 | 166%, 0 | 133%, 0 |
| HEVC 1920x1080 30 fps | 103%, 0 | 53%, 0 | 129%, 0 | 120%, 0 |
| AV1 1920x1080 60 fps | 156%, 0 | 50%, 0 | 158%, 0.1% | 100%, 0 |
| VP9 1920x1080 60 fps | 143%, 0 | 55%, 0 | 195%, 0 | 156%, 0 |
| H.264 3840x2160 30 fps | 201%, 0 | 55%, 0 | 255%, 0 | 202%, 0 |
| HEVC 10-bit 3840x2160 60 fps | 380%, 33% | 128%, 0 | 730%, 8% | 585%, 0.1% |
| AV1 10-bit 3840x2160 30 fps | 365%, 22% | 102%, 0 | 353%, 1.6% | 181%, 10% |

- The backend's own counters on the image path: the copy into the image
  costs about 1.2 ms a frame at 720p, 2.4 to 4.2 ms at 1080p and 13 to
  22 ms at 4K, plus 0.3 to 0.8 ms (5 to 7.6 ms at 4K) in `paint_image`.
  With NV12 both are 0.01 ms or less, because the mapped GStreamer buffer
  goes to the renderer as it is.
- With NVDEC the image path cannot keep up with 4K 10-bit: the BGRA
  conversion of a 10-bit frame alone takes 55 to 85 ms (`measure.py`),
  so a third of the frames drop. NV12 plays them with no drops. In
  software, NV12 still converts 10-bit to 8-bit on the CPU, which is why
  4K AV1 10-bit drops 10% there; P010 in the renderer would remove that.
- Memory falls too: 10 to 30% less with NV12 (no BGRA image per frame).
- 10 minute soak, 1080p60 H.264 with NVDEC: the image path showed 35,709
  frames, dropped 154 (0.4%), CPU 142%, memory +1.4 MB; NV12 showed 35,742,
  dropped 130, CPU 52%, memory +2.1 MB. Drift stayed within 33 ms (two
  frames at 60 fps) in both, and memory is flat.
- Time to first frame: 50 to 260 ms, except about 2 s for the first clip
  of each run (NVDEC and CUDA starting up).
- Software decode costs this machine's CPU per frame (`measure.py`, CPU ms
  per frame, best of three, one thread converting): H.264 720p 5.9, 1080p
  9.1, 1080p60 5.3, HEVC 1080p 15.0, H.264 4K 20.7, HEVC 10-bit 4K 25.5,
  AV1 10-bit 4K 32.4. The conversion to BGRA adds 0.3 to 3 ms at 1080p,
  18 ms at 4K and 55 to 85 ms for 10-bit 4K. With NVDEC, `measure.py`
  cannot run as written: the decoder outputs CUDA memory, which
  `caps=video/x-raw` does not accept.

What is still not zero-copy: with NVDEC, each frame is copied from video
memory to system memory (the appsink asks for NV12 in system memory) and
then uploaded again by the renderer. That is most of what the NVDEC + NV12
column still costs, and what E5's dmabuf step removes.

**Two more GPUI findings.**

- With the throttle off, the window draws 170 times a second on an 85 Hz
  screen whatever the video's rate: GPUI's X11 refresh rate ignores the
  mode's DoubleScan flag (the virtual screen's 1024x768 mode has it), and
  the element asks for a frame every tick while it waits for the next
  video frame. Both waste CPU; the second is ours (section 4.3, patch 6).
- `apt` on OVH's NVIDIA image installs newer driver userspace than the
  loaded kernel module ("Driver/library version mismatch"), after which
  GStreamer registers no NVDEC decoders; `ovh/setup.sh` reloads the
  module DKMS built.

### The OVH harness (`ovh/`)

No SSH is needed (and the cloud container this ran from cannot reach port
22): `box.py launch` creates an instance whose user-data starts
`agent.py`, which takes job scripts from a Swift container, runs them in
order and streams their logs back. The instance deletes itself at its
deadline, after 20 idle minutes, or through a systemd timer if the agent
dies. `box.py` keeps a ledger of every instance and refuses a launch that
could pass the budget. Jobs: `setup.sh` (packages, Xorg on the GPU, NVDEC
check), `baseline.sh` (tier A, heavier clips, `measure.py`, soak),
`gpui.sh` (Xorg, Xvfb, llvmpipe and Weston, stock and zed#5), `e5.sh` and
`rerun.sh`. A10s were often unavailable in GRA11 ("No valid host was
found", for six hours on 2026-11-13); failed builds are not billed and
`--wait-capacity` keeps trying.

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

- NV12 into our GPUI (karyalabs/zed#7) is the Linux path to ship behind
  `karya-gpui`: it halves to quarters the CPU and plays 4K 10-bit with
  NVDEC where the image path drops a third of the frames. The image path
  stays as the fallback on stock gpui-pre.
- E5 step 2: take NVDEC and VA-API frames as dmabufs and import them with
  wgpu's `texture_from_dmabuf_fd`, removing the copy out of video memory
  and the upload back. Then P010 for 10-bit.
- Ask apps (and our own player element) to turn GPUI's inactive-window
  throttle off while video plays, and stop asking for a frame every tick
  while waiting for the next video frame (pace to the video's rate).
- Wayland on a real desktop (GPUI needs a compositor with a seat), and an
  Intel or AMD GPU with VA-API: not covered by OVH.
- GStreamer fills in colour for streams that state it and leaves it empty
  otherwise (seen on the untagged VP9, MKV and AV1 clips); check with
  tier B whether any decoder fills in a guess.
