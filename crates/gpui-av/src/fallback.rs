//! A backend made of other backends, tried in order.

use std::any::Any;
use std::cell::RefCell;
use std::ops::Range;
use std::rc::{Rc, Weak};
use std::time::Duration;

use gpui::{App, SharedString, Window};

use crate::backend::{Backend, BackendEvent, EventSink, Placement, Session, Stats};
use crate::{Error, ErrorKind, PlaybackState, Source, Subtitles, TrackKind};

/// Plays each source with the first of several backends that can.
///
/// A backend is passed over when it refuses the source with
/// [`ErrorKind::Unsupported`], either at once from [`Backend::open`] or
/// while loading, before the media is ready. The next backend then opens
/// the same source, and the commands the app gave so far (play or pause,
/// a seek, volume, mute, looping) are given to it again. Any other failure
/// is final: a missing file or a refused token would fail on every backend
/// alike. Once one backend has the media ready (any state but loading or
/// buffering), it keeps it.
///
/// Media info is held back until a backend has the media ready, so a
/// player never shows a picture size or duration from a backend that then
/// gave up. [`VideoPlayer::backend_name`](crate::VideoPlayer::backend_name)
/// names the backend that is playing.
///
/// ```ignore
/// let backend = Fallback::new([
///     Rc::new(AvFoundationBackend::new()) as Rc<dyn Backend>,
///     Rc::new(GStreamerBackend::new()),
/// ]);
/// gpui_av::set_default_backend(Rc::new(backend), cx);
/// ```
pub struct Fallback {
    backends: Vec<Rc<dyn Backend>>,
}

impl Fallback {
    /// Tries `backends` in this order.
    pub fn new(backends: impl IntoIterator<Item = Rc<dyn Backend>>) -> Self {
        Self {
            backends: backends.into_iter().collect(),
        }
    }

    /// The backends, in the order they are tried.
    pub fn backends(&self) -> &[Rc<dyn Backend>] {
        &self.backends
    }
}

impl Backend for Fallback {
    fn name(&self) -> &'static str {
        "fallback"
    }

    /// The first backend's presenter: the one most sources will use.
    fn presenter(&self) -> &'static str {
        self.backends
            .first()
            .map_or("unknown", |backend| backend.presenter())
    }

    fn open(
        &self,
        source: Source,
        events: EventSink,
        cx: &mut App,
    ) -> Result<Box<dyn Session>, Error> {
        let chain = Rc::new(RefCell::new(Chain {
            backends: self.backends.clone(),
            next: 0,
            source,
            events,
            current: None,
            attempt: 0,
            ready: false,
            held_info: None,
            refusals: Vec::new(),
            wanted: Wanted::default(),
            mounted: None,
        }));
        match Chain::open_next(&chain, cx) {
            Some(error) => Err(error),
            None => Ok(Box::new(FallbackSession { chain })),
        }
    }
}

/// The commands given so far, to give again to the next backend.
#[derive(Default)]
struct Wanted {
    playing: Option<bool>,
    seek: Option<Duration>,
    volume: Option<f32>,
    muted: Option<bool>,
    looping: Option<bool>,
    audio_track: Option<usize>,
    text_track: Option<Option<usize>>,
    subtitles: Option<Option<Subtitles>>,
}

struct Current {
    name: &'static str,
    presenter: &'static str,
    session: Box<dyn Session>,
}

struct Chain {
    backends: Vec<Rc<dyn Backend>>,
    /// The index of the backend to try after the current one.
    next: usize,
    source: Source,
    /// Where the events for the player go.
    events: EventSink,
    current: Option<Current>,
    /// Counts the backends opened, so events from one that was passed over
    /// are ignored.
    attempt: usize,
    /// A backend has the media ready; it is kept from now on.
    ready: bool,
    held_info: Option<BackendEvent>,
    /// Why each backend passed so far said no, for the final error.
    refusals: Vec<String>,
    wanted: Wanted,
    /// `Some` while the element shows the picture, holding what the current
    /// session returned from `mount`.
    mounted: Option<Option<Box<dyn Any>>>,
}

impl Chain {
    /// Opens the source with the next backend that does not refuse it at
    /// once. Returns the error to fail with when none is left.
    fn open_next(chain: &Rc<RefCell<Self>>, cx: &mut App) -> Option<Error> {
        loop {
            let (backend, source, attempt) = {
                let mut this = chain.borrow_mut();
                // The session passed over goes first, with what it mounted.
                if let Some(mounted) = &mut this.mounted {
                    mounted.take();
                }
                this.current = None;
                let Some(backend) = this.backends.get(this.next).cloned() else {
                    return Some(this.final_error());
                };
                this.next += 1;
                this.attempt += 1;
                (backend, this.source.clone(), this.attempt)
            };
            let (sink, receiver) = EventSink::channel();
            match backend.open(source, sink, cx) {
                Ok(mut session) => {
                    let mut this = chain.borrow_mut();
                    this.wanted.replay(session.as_mut());
                    if this.mounted.is_some() {
                        this.mounted = Some(session.mount());
                    }
                    this.current = Some(Current {
                        name: backend.name(),
                        presenter: backend.presenter(),
                        session,
                    });
                    drop(this);
                    let weak = Rc::downgrade(chain);
                    cx.spawn(async move |cx| {
                        while let Ok(event) = receiver.recv().await {
                            let Some(chain) = weak.upgrade() else { break };
                            cx.update(|cx| Chain::handle(&chain, attempt, event, cx));
                        }
                    })
                    .detach();
                    return None;
                }
                Err(error) if error.kind() == ErrorKind::Unsupported => {
                    chain
                        .borrow_mut()
                        .refusals
                        .push(format!("{}: {error}", backend.name()));
                }
                Err(error) => return Some(error),
            }
        }
    }

    fn handle(chain: &Rc<RefCell<Self>>, attempt: usize, event: BackendEvent, cx: &mut App) {
        let mut this = chain.borrow_mut();
        if attempt != this.attempt {
            return;
        }
        if this.ready {
            this.events.send(event);
            return;
        }
        match event {
            BackendEvent::Info(_) => this.held_info = Some(event),
            // Waiting for data before the media is ready is still loading:
            // Media Foundation reports it as soon as play is asked for,
            // ahead of finding it has no decoder for the media.
            BackendEvent::State(PlaybackState::Loading | PlaybackState::Buffering) => {}
            BackendEvent::State(PlaybackState::Failed(error))
                if error.kind() == ErrorKind::Unsupported =>
            {
                let name = this.current.as_ref().map_or("?", |current| current.name);
                this.refusals.push(format!("{name}: {error}"));
                this.held_info = None;
                drop(this);
                if let Some(error) = Chain::open_next(chain, cx) {
                    chain.borrow().events.fail(error);
                }
            }
            BackendEvent::State(PlaybackState::Failed(error)) => this.events.fail(error),
            BackendEvent::State(state) => {
                this.ready = true;
                if let Some(info) = this.held_info.take() {
                    this.events.send(info);
                }
                this.events.state(state);
            }
            BackendEvent::Seeked(_) => this.events.send(event),
        }
    }

    fn final_error(&self) -> Error {
        let message = if self.refusals.is_empty() {
            "no backend to try".to_string()
        } else {
            format!("no backend can play this: {}", self.refusals.join("; "))
        };
        Error::new(ErrorKind::Unsupported, message)
    }
}

impl Wanted {
    fn replay(&self, session: &mut dyn Session) {
        if let Some(volume) = self.volume {
            session.set_volume(volume);
        }
        if let Some(muted) = self.muted {
            session.set_muted(muted);
        }
        if let Some(looping) = self.looping {
            session.set_looping(looping);
        }
        if let Some(index) = self.audio_track {
            session.select_track(TrackKind::Audio, Some(index));
        }
        if let Some(index) = self.text_track {
            session.select_track(TrackKind::Text, index);
        }
        if let Some(subtitles) = &self.subtitles {
            session.set_subtitles(subtitles.as_ref());
        }
        if let Some(position) = self.seek {
            session.seek(position);
        }
        match self.playing {
            Some(true) => session.play(),
            Some(false) => session.pause(),
            None => {}
        }
    }
}

struct FallbackSession {
    chain: Rc<RefCell<Chain>>,
}

impl FallbackSession {
    fn with<R>(&self, read: impl FnOnce(&dyn Session) -> R) -> Option<R> {
        let chain = self.chain.borrow();
        chain
            .current
            .as_ref()
            .map(|current| read(current.session.as_ref()))
    }

    fn command(&mut self, record: impl FnOnce(&mut Wanted), give: impl FnOnce(&mut dyn Session)) {
        let mut chain = self.chain.borrow_mut();
        if !chain.ready {
            record(&mut chain.wanted);
        }
        if let Some(current) = &mut chain.current {
            give(current.session.as_mut());
        }
    }
}

/// What the element keeps while it shows the picture.
struct MountGuard(Weak<RefCell<Chain>>);

impl Drop for MountGuard {
    fn drop(&mut self) {
        if let Some(chain) = self.0.upgrade()
            && let Ok(mut chain) = chain.try_borrow_mut()
        {
            chain.mounted = None;
        }
    }
}

impl Session for FallbackSession {
    fn play(&mut self) {
        self.command(
            |wanted| wanted.playing = Some(true),
            |session| session.play(),
        );
    }

    fn pause(&mut self) {
        self.command(
            |wanted| wanted.playing = Some(false),
            |session| session.pause(),
        );
    }

    fn seek(&mut self, position: Duration) {
        self.command(
            |wanted| wanted.seek = Some(position),
            |session| session.seek(position),
        );
    }

    fn set_volume(&mut self, volume: f32) {
        self.command(
            |wanted| wanted.volume = Some(volume),
            |session| session.set_volume(volume),
        );
    }

    fn set_muted(&mut self, muted: bool) {
        self.command(
            |wanted| wanted.muted = Some(muted),
            |session| session.set_muted(muted),
        );
    }

    fn set_looping(&mut self, looping: bool) {
        self.command(
            |wanted| wanted.looping = Some(looping),
            |session| session.set_looping(looping),
        );
    }

    fn text(&self) -> Option<SharedString> {
        self.with(|session| session.text()).flatten()
    }

    fn set_subtitles(&mut self, subtitles: Option<&Subtitles>) {
        self.command(
            |wanted| wanted.subtitles = Some(subtitles.cloned()),
            |session| session.set_subtitles(subtitles),
        );
    }

    fn draws_subtitles(&self) -> bool {
        self.with(|session| session.draws_subtitles())
            .unwrap_or(false)
    }

    fn select_track(&mut self, kind: TrackKind, index: Option<usize>) {
        self.command(
            |wanted| match kind {
                TrackKind::Audio => wanted.audio_track = index,
                TrackKind::Text => wanted.text_track = Some(index),
            },
            |session| session.select_track(kind, index),
        );
    }

    fn position(&self) -> Duration {
        self.with(|session| session.position()).unwrap_or_default()
    }

    fn buffered(&self) -> Vec<Range<Duration>> {
        self.with(|session| session.buffered()).unwrap_or_default()
    }

    fn stats(&self) -> Stats {
        self.with(|session| session.stats()).unwrap_or_default()
    }

    fn backend_name(&self) -> Option<&'static str> {
        let chain = self.chain.borrow();
        let current = chain.current.as_ref()?;
        Some(current.session.backend_name().unwrap_or(current.name))
    }

    fn presenter(&self) -> Option<&'static str> {
        let chain = self.chain.borrow();
        let current = chain.current.as_ref()?;
        Some(current.session.presenter().unwrap_or(current.presenter))
    }

    fn mount(&mut self) -> Option<Box<dyn Any>> {
        let mut chain = self.chain.borrow_mut();
        let inner = chain
            .current
            .as_mut()
            .and_then(|current| current.session.mount());
        chain.mounted = Some(inner);
        Some(Box::new(MountGuard(Rc::downgrade(&self.chain))))
    }

    fn needs_frames(&self) -> bool {
        self.with(|session| session.needs_frames()).unwrap_or(false)
    }

    fn paint(&mut self, placement: Placement, window: &mut Window, cx: &mut App) {
        let mut chain = self.chain.borrow_mut();
        if let Some(current) = &mut chain.current {
            current.session.paint(placement, window, cx);
        }
    }
}
