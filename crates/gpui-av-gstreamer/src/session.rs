//! One piece of media playing through GStreamer's `playbin`.

use std::cell::RefCell;
use std::ffi::c_int;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use gpui::{App, Corners, SharedString, Task, Window};
use gpui_av::{
    Error, ErrorKind, EventSink, MediaInfo, Placement, PlaybackState, Rotation, Session, Stats,
    TrackKind,
};

use crate::ffi;
use crate::pipeline::{Event, Picture, Pipeline};

/// How often the session reads the pipeline's bus. Frames do not wait for
/// this: they reach the session from GStreamer's own thread.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// How long the session keeps asking for window frames while it waits for
/// a picture that has not arrived (the first frame, or the frame after a
/// seek while paused).
const FRAME_WAIT: Duration = Duration::from_secs(3);

/// A stream that ends this far before its stated length is cut off.
const TRUNCATED: Duration = Duration::from_secs(1);

/// How long after a seek, a loop or a resume drift is not measured.
const SETTLE: Duration = Duration::from_millis(250);

pub(crate) struct GstSession {
    inner: Rc<RefCell<Inner>>,
    _poll: Task<()>,
}

struct Inner {
    pipeline: Pipeline,
    events: EventSink,
    /// The last state sent, so a state is reported once.
    reported: PlaybackState,
    /// The media prerolled: its properties are known.
    loaded: bool,
    failed: bool,
    /// The app asked for playback or a pause.
    started: bool,
    /// The app wants the media to play.
    wants_play: bool,
    ended: bool,
    looping: bool,
    buffering: bool,
    /// The pipeline's own state.
    current: c_int,
    /// A seek asked for before the media loaded.
    pending_seek: Option<Duration>,
    /// Track choices made before the streams were known.
    pending_tracks: Vec<(TrackKind, Option<usize>)>,
    /// Whether a text track was chosen; text inside the media starts off.
    text_on: bool,
    /// The latest cue from the text track: its text, start and end.
    cue: Option<(String, Option<Duration>, Option<Duration>)>,
    /// The seek in flight.
    seeking: Option<Duration>,
    rotation: Option<Rotation>,
    missing_plugin: Option<String>,
    /// The frame on screen, and its presentation time.
    shown: Option<(Option<Duration>, Picture)>,
    /// Pictures to free from GPUI's atlas at the next paint.
    retired: Vec<Picture>,
    presented: u64,
    replaced: u64,
    sink_dropped: u64,
    sink_dropped_seen: u64,
    drift_from: Option<Instant>,
    max_drift: Option<Duration>,
    awaiting_frame: Option<Instant>,
    /// Time spent in `paint_image` for new frames (GPUI's upload), for
    /// the E2 numbers.
    upload: Duration,
    uploads: u64,
}

impl GstSession {
    pub(crate) fn new(pipeline: Pipeline, events: EventSink, cx: &mut App) -> Self {
        // A refusal here also lands on the bus, which the first poll reads.
        pipeline.set_state(ffi::GST_STATE_PAUSED);
        let inner = Rc::new(RefCell::new(Inner {
            pipeline,
            events,
            reported: PlaybackState::Loading,
            loaded: false,
            failed: false,
            started: false,
            wants_play: false,
            ended: false,
            looping: false,
            buffering: false,
            current: ffi::GST_STATE_NULL,
            pending_seek: None,
            pending_tracks: Vec::new(),
            text_on: false,
            cue: None,
            seeking: None,
            rotation: None,
            missing_plugin: None,
            shown: None,
            retired: Vec::new(),
            presented: 0,
            replaced: 0,
            sink_dropped: 0,
            sink_dropped_seen: 0,
            drift_from: None,
            max_drift: None,
            awaiting_frame: Some(Instant::now()),
            upload: Duration::ZERO,
            uploads: 0,
        }));
        let poll = cx.spawn({
            let inner = Rc::downgrade(&inner);
            async move |cx| {
                loop {
                    let Some(inner) = inner.upgrade() else { break };
                    if let Ok(mut inner) = inner.try_borrow_mut() {
                        inner.poll();
                        if inner.failed {
                            break;
                        }
                    }
                    drop(inner);
                    cx.background_executor().timer(POLL_INTERVAL).await;
                }
            }
        });
        Self { inner, _poll: poll }
    }
}

impl Inner {
    fn poll(&mut self) {
        if self.failed {
            return;
        }
        if self.pipeline.headers_refused() {
            self.fail(Error::new(
                ErrorKind::Unsupported,
                "GStreamer's source for this URL cannot send request headers",
            ));
            return;
        }
        if let Some(error) = self.truncated() {
            self.fail(error);
            return;
        }
        while let Some(cue) = self.pipeline.take_cue() {
            self.cue = Some(cue);
        }
        while let Some(message) = self.pipeline.pop() {
            match self.pipeline.read(&message) {
                Event::Error(error) => {
                    let error = match self.missing_plugin.take() {
                        Some(plugin) if error.kind() != ErrorKind::NotFound => Error::new(
                            ErrorKind::Unsupported,
                            format!("GStreamer needs {plugin}, which is not installed"),
                        ),
                        _ => error,
                    };
                    self.fail(error);
                    return;
                }
                // playbin plays what it can and leaves out the rest without
                // an error, so a missing picture or sound decoder would
                // look like a clip without that track. Refuse it instead,
                // so a fallback can try another backend.
                Event::MissingPlugin {
                    name,
                    essential: true,
                } => {
                    self.fail(Error::new(
                        ErrorKind::Unsupported,
                        format!("GStreamer needs {name}, which is not installed"),
                    ));
                    return;
                }
                Event::MissingPlugin { name, .. } => self.missing_plugin = Some(name),
                Event::Eos => self.on_end(),
                Event::Rotation(rotation) => {
                    if self.rotation != Some(rotation) {
                        self.rotation = Some(rotation);
                        if self.loaded {
                            self.send_info();
                        }
                    }
                }
                Event::Buffering(percent) => {
                    let buffering = percent < 100;
                    if buffering != self.buffering {
                        self.buffering = buffering;
                        // Hold the clock while the queue fills, as
                        // GStreamer asks of apps playing network streams.
                        if self.wants_play {
                            self.pipeline.set_state(if buffering {
                                ffi::GST_STATE_PAUSED
                            } else {
                                ffi::GST_STATE_PLAYING
                            });
                        }
                    }
                }
                Event::State(state) => self.current = state,
                Event::AsyncDone => {
                    if !self.loaded {
                        self.on_loaded();
                    } else if let Some(position) = self.seeking.take() {
                        self.events.seeked(position);
                        self.awaiting_frame = Some(Instant::now());
                    }
                }
                Event::DurationChanged => {
                    if self.loaded {
                        self.send_info();
                    }
                }
                Event::ClockLost => {
                    // GStreamer's advice: pause and play again to pick a
                    // new clock.
                    if self.wants_play {
                        self.pipeline.set_state(ffi::GST_STATE_PAUSED);
                        self.pipeline.set_state(ffi::GST_STATE_PLAYING);
                    }
                }
                Event::SinkDropped(dropped) => {
                    // The count restarts after a flush.
                    self.sink_dropped += dropped
                        .checked_sub(self.sink_dropped_seen)
                        .unwrap_or(dropped);
                    self.sink_dropped_seen = dropped;
                }
                Event::Other => {}
            }
        }
        let state = if !self.loaded {
            PlaybackState::Loading
        } else if self.ended {
            PlaybackState::Ended
        } else if self.wants_play {
            if self.buffering {
                PlaybackState::Buffering
            } else if self.current == ffi::GST_STATE_PLAYING
                || self.reported == PlaybackState::Playing
            {
                PlaybackState::Playing
            } else {
                // On its way to playing; say so once it gets there.
                self.reported.clone()
            }
        } else if self.started {
            PlaybackState::Paused
        } else {
            PlaybackState::Ready
        };
        self.report(state);
    }

    /// GStreamer's demuxers end a file that is cut off without an error,
    /// and the pipeline only says so when the clock reaches the stated
    /// length. A picture that ends well before that length is a cut-off
    /// file, said as soon as the picture ends.
    fn truncated(&self) -> Option<Error> {
        let shared = &self.pipeline.shared;
        if !shared.video_ended.load(Ordering::Acquire) || self.seeking.is_some() {
            return None;
        }
        let last = shared.last_pts()?;
        let duration = self.pipeline.duration()?;
        (duration.saturating_sub(last) > TRUNCATED).then(|| {
            Error::new(
                ErrorKind::Decode,
                format!(
                    "the picture stops at {:.1} s of {:.1} s: the file is cut off or damaged",
                    last.as_secs_f64(),
                    duration.as_secs_f64()
                ),
            )
        })
    }

    fn report(&mut self, state: PlaybackState) {
        if state != self.reported {
            self.reported = state.clone();
            self.events.state(state);
        }
    }

    fn fail(&mut self, error: Error) {
        self.failed = true;
        self.awaiting_frame = None;
        self.pipeline.set_state(ffi::GST_STATE_NULL);
        self.report(PlaybackState::Failed(error));
    }

    fn apply_track(&mut self, kind: TrackKind, index: Option<usize>) {
        if kind == TrackKind::Text {
            self.text_on = index.is_some();
        }
        self.pipeline.select_track(kind, index);
    }

    fn on_loaded(&mut self) {
        self.loaded = true;
        for (kind, index) in std::mem::take(&mut self.pending_tracks) {
            self.apply_track(kind, index);
        }
        self.send_info();
        if let Some(position) = self.pending_seek.take() {
            self.seek_to(position);
        }
        if self.wants_play {
            self.pipeline.set_state(ffi::GST_STATE_PLAYING);
        }
    }

    fn send_info(&mut self) {
        let has_video = self.pipeline.video_streams() > 0;
        if has_video && let Some(rotation) = self.pipeline.video_rotation() {
            self.rotation = Some(rotation);
        }
        let size = has_video
            .then(|| self.pipeline.display_size())
            .flatten()
            .map(|(width, height)| gpui::size(width, height));
        if !has_video {
            self.awaiting_frame = None;
        }
        let info = MediaInfo {
            duration: self.pipeline.duration(),
            natural_size: size,
            rotation: has_video.then(|| self.rotation.unwrap_or_default()),
            has_video,
            has_audio: Some(self.pipeline.audio_streams() > 0),
            color: if has_video {
                self.pipeline.color()
            } else {
                Default::default()
            },
            tracks: self.pipeline.tracks(),
            audio_track: self.pipeline.audio_track(),
            text_track: self.text_on.then(|| self.pipeline.text_track()).flatten(),
        };
        self.events.info(info);
    }

    fn on_end(&mut self) {
        if self.looping {
            self.seek_to(Duration::ZERO);
            return;
        }
        self.ended = true;
        self.wants_play = false;
        self.pipeline.set_state(ffi::GST_STATE_PAUSED);
    }

    fn seek_to(&mut self, position: Duration) {
        let position = match self.pipeline.duration() {
            Some(duration) => position.min(duration),
            None => position,
        };
        self.pipeline
            .shared
            .video_ended
            .store(false, Ordering::Release);
        if self.pipeline.seek(position) {
            self.seeking = Some(position);
        } else {
            // Not seekable (a live stream): report the seek as done where
            // the media is.
            self.events
                .seeked(self.pipeline.position().unwrap_or_default());
        }
        self.drift_from = Some(Instant::now() + SETTLE);
        self.awaiting_frame = Some(Instant::now());
    }

    fn take_frame(&mut self) {
        let (frame, replaced) = {
            let mut frames = self
                .pipeline
                .shared
                .frames
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            (frames.latest.take(), std::mem::take(&mut frames.replaced))
        };
        self.replaced += replaced;
        let Some(frame) = frame else { return };
        self.awaiting_frame = None;
        if self
            .shown
            .as_ref()
            .is_some_and(|(pts, _)| *pts == frame.pts && frame.pts.is_some())
        {
            // The preroll frame again, as the first sample.
            return;
        }
        self.presented += 1;
        if self.wants_play
            && self.seeking.is_none()
            && self.drift_from.is_none_or(|from| Instant::now() >= from)
            && let (Some(pts), Some(position)) = (frame.pts, self.pipeline.position())
        {
            let drift = position.abs_diff(pts);
            self.max_drift = Some(self.max_drift.unwrap_or_default().max(drift));
        }
        if let Some((_, old)) = self.shown.replace((frame.pts, frame.picture)) {
            self.retired.push(old);
        }
    }
}

impl Session for GstSession {
    fn play(&mut self) {
        let mut inner = self.inner.borrow_mut();
        if inner.failed {
            return;
        }
        if inner.ended {
            inner.ended = false;
            if inner.loaded {
                inner.seek_to(Duration::ZERO);
            }
        }
        inner.started = true;
        inner.wants_play = true;
        inner.drift_from = Some(Instant::now() + SETTLE);
        if !inner.buffering {
            inner.pipeline.set_state(ffi::GST_STATE_PLAYING);
        }
        inner.poll();
    }

    fn pause(&mut self) {
        let mut inner = self.inner.borrow_mut();
        if inner.failed {
            return;
        }
        inner.started = true;
        inner.wants_play = false;
        inner.pipeline.set_state(ffi::GST_STATE_PAUSED);
        inner.poll();
    }

    fn seek(&mut self, position: Duration) {
        let mut inner = self.inner.borrow_mut();
        if inner.failed {
            return;
        }
        if inner.ended {
            inner.ended = false;
        }
        if inner.loaded {
            inner.seek_to(position);
        } else {
            inner.pending_seek = Some(position);
        }
        inner.poll();
    }

    fn set_volume(&mut self, volume: f32) {
        self.inner.borrow().pipeline.set_volume(volume);
    }

    fn set_muted(&mut self, muted: bool) {
        self.inner.borrow().pipeline.set_muted(muted);
    }

    fn set_looping(&mut self, looping: bool) {
        self.inner.borrow_mut().looping = looping;
    }

    fn select_track(&mut self, kind: TrackKind, index: Option<usize>) {
        let mut inner = self.inner.borrow_mut();
        if !inner.loaded {
            inner.pending_tracks.push((kind, index));
            return;
        }
        inner.apply_track(kind, index);
        inner.send_info();
    }

    fn text(&self) -> Option<SharedString> {
        let position = self.position();
        let inner = self.inner.borrow();
        if !inner.text_on {
            return None;
        }
        let (text, start, end) = inner.cue.as_ref()?;
        let started = start.is_none_or(|start| start <= position);
        let ended = end.is_some_and(|end| position >= end);
        (started && !ended && !text.is_empty()).then(|| SharedString::from(text.clone()))
    }

    fn position(&self) -> Duration {
        let inner = self.inner.borrow();
        if !inner.loaded {
            return inner.pending_seek.unwrap_or_default();
        }
        if let Some(position) = inner.seeking {
            return position;
        }
        if inner.ended
            && let Some(duration) = inner.pipeline.duration()
        {
            return duration;
        }
        inner.pipeline.position().unwrap_or_default()
    }

    fn buffered(&self) -> Vec<std::ops::Range<Duration>> {
        let inner = self.inner.borrow();
        if !inner.loaded {
            return Vec::new();
        }
        inner.pipeline.buffered()
    }

    fn stats(&self) -> Stats {
        let inner = self.inner.borrow();
        let stats = Stats::new(inner.presented, inner.replaced + inner.sink_dropped);
        match inner.max_drift {
            Some(drift) => stats.with_max_drift(drift),
            None => stats,
        }
    }

    fn paint(&mut self, placement: Placement, window: &mut Window, _cx: &mut App) {
        let mut inner = self.inner.borrow_mut();
        if inner.failed {
            return;
        }
        let shown = inner.presented;
        inner.take_frame();
        let new_frame = inner.presented != shown;
        let started = Instant::now();
        for picture in inner.retired.drain(..) {
            // Frees the old frame's texture; without this every frame
            // would stay in GPUI's atlas. A video frame's textures belong
            // to the renderer, which reuses them.
            if let Picture::Image(image) = picture
                && let Err(error) = window.drop_image(image)
            {
                log(&format!("could not free a frame: {error}"));
            }
        }
        match inner.shown.as_ref().map(|(_, picture)| picture) {
            Some(Picture::Image(image)) => {
                if let Err(error) = window.paint_image(
                    placement.bounds,
                    placement.bounds,
                    Corners::default(),
                    image.clone(),
                    0,
                    false,
                ) {
                    log(&format!("could not draw a frame: {error}"));
                }
            }
            #[cfg(feature = "karya-gpui")]
            Some(Picture::Video(frame)) => {
                window.paint_video_frame(placement.bounds, frame.clone());
            }
            None => {}
        }
        if new_frame {
            inner.upload += started.elapsed();
            inner.uploads += 1;
        }
        if inner
            .awaiting_frame
            .is_some_and(|since| since.elapsed() < FRAME_WAIT)
        {
            window.request_animation_frame();
        }
    }
}

impl Drop for GstSession {
    fn drop(&mut self) {
        // E2's per-frame costs, on request (experiments/e2-gstreamer).
        if std::env::var_os("GPUI_AV_GSTREAMER_STATS").is_none() {
            return;
        }
        let inner = self.inner.borrow();
        let shared = &inner.pipeline.shared;
        let copies = shared.copies.load(Ordering::Relaxed);
        let copy = Duration::from_nanos(shared.copy_nanos.load(Ordering::Relaxed));
        let per_frame =
            |total: Duration, count: u64| total.as_secs_f64() * 1000.0 / count.max(1) as f64;
        log(&format!(
            "{copies} frames copied at {:.2} ms each; {} drawn, {:.2} ms each in {}",
            per_frame(copy, copies),
            inner.uploads,
            per_frame(inner.upload, inner.uploads),
            if cfg!(feature = "karya-gpui") && std::env::var_os("GPUI_AV_DMABUF").is_some() {
                "paint_video_frame (dmabuf, no copy)"
            } else if cfg!(feature = "karya-gpui") {
                "paint_video_frame (NV12, no copy)"
            } else {
                "paint_image"
            },
        ));
    }
}

/// The lab installs no logger, so the few things worth saying go to
/// standard error, as the other backends do.
fn log(message: &str) {
    eprintln!("gpui-av-gstreamer: {message}");
}
