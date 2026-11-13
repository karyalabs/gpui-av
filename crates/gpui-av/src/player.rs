use std::any::Any;
use std::rc::Rc;
use std::time::Duration;

use gpui::{App, Context, EventEmitter, Size, Task, Window};

use crate::backend::{
    Backend, BackendEvent, EventSink, Placement, Session, Stats, default_backend,
};
use crate::{Error, ErrorKind, MediaInfo, Source};

/// Where a player is in the life of its media.
#[derive(Clone, Debug, PartialEq)]
pub enum PlaybackState {
    /// The source is being opened; nothing can be shown yet.
    Loading,
    /// The media is loaded and has not been started.
    Ready,
    /// Playing.
    Playing,
    /// Paused by the app.
    Paused,
    /// Playback is wanted but stalled, waiting for data.
    Buffering,
    /// Played to the end (never reached while looping).
    Ended,
    /// Playback failed and will not continue.
    Failed(Error),
}

/// What a [`VideoPlayer`] emits. Subscribe with `cx.subscribe(&player, ..)`.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum PlayerEvent {
    /// [`VideoPlayer::state`] changed to this.
    StateChanged(PlaybackState),
    /// [`VideoPlayer::info`] changed (duration, size and so on).
    InfoChanged,
    /// A seek finished at this position.
    Seeked(Duration),
}

/// A piece of media and its playback, as a GPUI entity.
///
/// Create one with `cx.new(|cx| VideoPlayer::new(source, cx))`, draw it with
/// [`video()`](crate::video), and control it with the methods here. The
/// entity notifies its observers whenever its state or media info changes.
pub struct VideoPlayer {
    backend_name: &'static str,
    presenter: &'static str,
    /// The session's last stats, kept for after a failure drops it.
    final_stats: Stats,
    session: Option<Box<dyn Session>>,
    state: PlaybackState,
    info: MediaInfo,
    volume: f32,
    muted: bool,
    looping: bool,
    _events: Option<Task<()>>,
}

impl VideoPlayer {
    /// Opens `source` with the default backend (see
    /// [`set_default_backend`](crate::set_default_backend)). With no default
    /// set, the player starts in [`PlaybackState::Failed`].
    pub fn new(source: Source, cx: &mut Context<Self>) -> Self {
        match default_backend(cx) {
            Some(backend) => Self::with_backend(source, backend, cx),
            None => Self::failed(
                "none",
                "none",
                Error::new(
                    ErrorKind::NoBackend,
                    "no gpui-av backend is set; call gpui_av::set_default_backend at startup",
                ),
            ),
        }
    }

    /// Opens `source` with a backend of the app's choice.
    pub fn with_backend(source: Source, backend: Rc<dyn Backend>, cx: &mut Context<Self>) -> Self {
        let (name, presenter) = (backend.name(), backend.presenter());
        let (sink, events) = EventSink::channel();
        let session = match backend.open(source, sink, cx) {
            Ok(session) => session,
            Err(error) => return Self::failed(name, presenter, error),
        };
        let pump = cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let alive = this.update(cx, |this, cx| this.handle_event(event, cx));
                if alive.is_err() {
                    break;
                }
            }
        });
        Self {
            backend_name: name,
            presenter,
            final_stats: Stats::default(),
            session: Some(session),
            state: PlaybackState::Loading,
            info: MediaInfo::default(),
            volume: 1.0,
            muted: false,
            looping: false,
            _events: Some(pump),
        }
    }

    fn failed(backend_name: &'static str, presenter: &'static str, error: Error) -> Self {
        Self {
            backend_name,
            presenter,
            final_stats: Stats::default(),
            session: None,
            state: PlaybackState::Failed(error),
            info: MediaInfo::default(),
            volume: 1.0,
            muted: false,
            looping: false,
            _events: None,
        }
    }

    fn handle_event(&mut self, event: BackendEvent, cx: &mut Context<Self>) {
        // A failed player stays failed, whatever a backend sends afterwards.
        if matches!(self.state, PlaybackState::Failed(_)) {
            return;
        }
        match event {
            BackendEvent::Info(info) => {
                if self.info != info {
                    self.info = info;
                    cx.emit(PlayerEvent::InfoChanged);
                    cx.notify();
                }
            }
            BackendEvent::State(state) => {
                if self.state != state {
                    if matches!(state, PlaybackState::Failed(_)) {
                        // Nothing more can play; stop the backend's work,
                        // keeping what it counted.
                        if let Some(session) = self.session.take() {
                            self.final_stats = session.stats();
                        }
                    }
                    self.state = state.clone();
                    cx.emit(PlayerEvent::StateChanged(state));
                    cx.notify();
                }
            }
            BackendEvent::Seeked(position) => {
                cx.emit(PlayerEvent::Seeked(position));
                cx.notify();
            }
        }
    }

    /// Starts or resumes playback. After the end, plays again from the start.
    pub fn play(&mut self) {
        if let Some(session) = &mut self.session {
            session.play();
        }
    }

    /// Pauses playback, keeping the position.
    pub fn pause(&mut self) {
        if let Some(session) = &mut self.session {
            session.pause();
        }
    }

    /// Moves to `position`, clamped to the media's length. A
    /// [`PlayerEvent::Seeked`] follows when the new frame is ready.
    pub fn seek(&mut self, position: Duration) {
        if let Some(session) = &mut self.session {
            session.seek(position);
        }
    }

    /// Sets the volume, from 0.0 (silent) to 1.0 (full). Values outside that
    /// range are clamped.
    pub fn set_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        let volume = if volume.is_nan() {
            0.0
        } else {
            volume.clamp(0.0, 1.0)
        };
        self.volume = volume;
        if let Some(session) = &mut self.session {
            session.set_volume(volume);
        }
        cx.notify();
    }

    /// Mutes or unmutes the sound without changing the volume.
    pub fn set_muted(&mut self, muted: bool, cx: &mut Context<Self>) {
        self.muted = muted;
        if let Some(session) = &mut self.session {
            session.set_muted(muted);
        }
        cx.notify();
    }

    /// When looping, the media starts again at its end instead of ending.
    pub fn set_looping(&mut self, looping: bool, cx: &mut Context<Self>) {
        self.looping = looping;
        if let Some(session) = &mut self.session {
            session.set_looping(looping);
        }
        cx.notify();
    }

    /// The playback state.
    pub fn state(&self) -> &PlaybackState {
        &self.state
    }

    /// The current position. Zero before the media has loaded and after a
    /// failure.
    pub fn position(&self) -> Duration {
        self.session
            .as_ref()
            .map_or(Duration::ZERO, |session| session.position())
    }

    /// How well the picture keeps up: frames presented and dropped, as the
    /// backend counts them. All zero when the backend cannot tell. After a
    /// failure, the counts as they stood when playback failed.
    pub fn stats(&self) -> Stats {
        self.session
            .as_ref()
            .map_or(self.final_stats, |session| session.stats())
    }

    /// The media's length, once known. `None` for media without one.
    pub fn duration(&self) -> Option<Duration> {
        self.info.duration
    }

    /// The size of the picture as shown, in pixels, once known.
    pub fn natural_size(&self) -> Option<Size<u32>> {
        self.info.natural_size
    }

    /// Everything the backend reported about the media.
    pub fn info(&self) -> &MediaInfo {
        &self.info
    }

    /// The volume, from 0.0 to 1.0.
    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// Whether the sound is muted.
    pub fn muted(&self) -> bool {
        self.muted
    }

    /// Whether the media loops.
    pub fn looping(&self) -> bool {
        self.looping
    }

    /// The name of the backend playing this media.
    pub fn backend_name(&self) -> &'static str {
        self.backend_name
    }

    /// How the backend gets frames on screen, such as `"surface"`.
    pub fn presenter(&self) -> &'static str {
        self.presenter
    }

    /// Whether the picture can change without a new event: the element keeps
    /// asking for frames while this holds.
    pub(crate) fn wants_frames(&self) -> bool {
        matches!(
            self.state,
            PlaybackState::Playing | PlaybackState::Buffering
        )
    }

    pub(crate) fn mount(&mut self) -> Option<Box<dyn Any>> {
        self.session.as_mut().and_then(|session| session.mount())
    }

    pub(crate) fn paint(&mut self, placement: Placement, window: &mut Window, cx: &mut App) {
        if let Some(session) = &mut self.session {
            session.paint(placement, window, cx);
        }
    }
}

impl EventEmitter<PlayerEvent> for VideoPlayer {}
