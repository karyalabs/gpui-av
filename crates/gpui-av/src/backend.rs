//! The contract between gpui-av and a decoder stack.
//!
//! A [`Backend`] opens a [`Source`] and returns a [`Session`]: one piece of
//! media being played. The session owns decoding, audio output and the
//! clock. It reports what happens through an [`EventSink`], takes commands
//! through its methods, and draws the current frame when asked.
//!
//! Nothing here names a platform type, so a backend can be written against
//! this module alone.

use std::rc::Rc;
use std::time::Duration;

use gpui::{App, Bounds, Global, Pixels, Window};

use crate::{Error, MediaInfo, PlaybackState, Source};

/// A decoder stack that can play media.
///
/// A backend is a factory; it holds no per-media state. An app sets one as
/// the default with [`set_default_backend`], or hands one to
/// [`VideoPlayer::with_backend`](crate::VideoPlayer::with_backend).
pub trait Backend: 'static {
    /// A short stable name for stats and logs, such as `"avfoundation"`.
    fn name(&self) -> &'static str;

    /// Starts loading `source`. Runs on the main thread and must not block:
    /// loading goes on in the background and its progress arrives through
    /// `events`.
    ///
    /// Return an error only when the backend can tell at once that it will
    /// not play this source (for example a kind of source it does not take).
    /// Failures found while loading are reported as
    /// [`PlaybackState::Failed`] through `events`.
    fn open(
        &self,
        source: Source,
        events: EventSink,
        cx: &mut App,
    ) -> Result<Box<dyn Session>, Error>;
}

/// One piece of media being played by a backend.
///
/// Every method runs on the main thread and must return quickly. Commands
/// may arrive in any state, including while the media is still loading; the
/// session applies them as soon as it can. Dropping the session stops
/// playback and frees everything it holds.
pub trait Session: 'static {
    /// Starts or resumes playback. After the media has ended, plays it again
    /// from the start.
    fn play(&mut self);

    /// Pauses playback, keeping the position.
    fn pause(&mut self);

    /// Moves to `position`, clamped to the media's length. The session sends
    /// [`BackendEvent::Seeked`] once the frame at the new position is ready.
    fn seek(&mut self, position: Duration);

    /// Sets the volume, from 0.0 (silent) to 1.0 (full).
    fn set_volume(&mut self, volume: f32);

    /// Mutes or unmutes the sound without changing the volume.
    fn set_muted(&mut self, muted: bool);

    /// When looping, the media starts again at its end instead of ending.
    fn set_looping(&mut self, looping: bool);

    /// The current position on the session's clock (the audio clock, where
    /// there is one).
    fn position(&self) -> Duration;

    /// Draws the frame that is due now. Called during the paint phase of
    /// every window frame in which the [`video()`](crate::video) element is
    /// on screen. The session presents the frame however its platform does
    /// it best: painting a GPU surface or an image into the window, or
    /// moving a native layer over `placement`.
    fn paint(&mut self, placement: Placement, window: &mut Window, cx: &mut App);
}

/// Where a session draws its frame, in window coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// Where the whole picture goes. gpui-av has already applied the
    /// element's object fit, so the session stretches the frame to exactly
    /// these bounds.
    pub bounds: Bounds<Pixels>,
    /// The part of the window the picture may show in. `bounds` can extend
    /// past it (with [`ObjectFit::Cover`](crate::ObjectFit::Cover)). What is
    /// painted into the window is clipped already; a session that presents
    /// outside the window's scene must clip to this itself.
    pub clip: Bounds<Pixels>,
}

/// What a session tells its player.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum BackendEvent {
    /// The media's properties are known, or have changed.
    Info(MediaInfo),
    /// The playback state changed.
    State(PlaybackState),
    /// A seek finished: the frame at this position is ready to draw.
    Seeked(Duration),
}

/// The channel a session reports through. Cheap to clone, and safe to use
/// from any thread, so a decoder callback can report directly.
#[derive(Clone, Debug)]
pub struct EventSink {
    sender: async_channel::Sender<BackendEvent>,
}

impl EventSink {
    pub(crate) fn channel() -> (Self, async_channel::Receiver<BackendEvent>) {
        let (sender, receiver) = async_channel::unbounded();
        (Self { sender }, receiver)
    }

    /// Sends an event to the player. Does nothing once the player is gone.
    pub fn send(&self, event: BackendEvent) {
        // The channel is unbounded, so the only failure is a closed channel,
        // which means nobody is listening any more.
        let _ = self.sender.try_send(event);
    }

    /// Reports the media's properties.
    pub fn info(&self, info: MediaInfo) {
        self.send(BackendEvent::Info(info));
    }

    /// Reports a new playback state.
    pub fn state(&self, state: PlaybackState) {
        self.send(BackendEvent::State(state));
    }

    /// Reports a failure; shorthand for the [`PlaybackState::Failed`] state.
    pub fn fail(&self, error: Error) {
        self.state(PlaybackState::Failed(error));
    }

    /// Reports that a seek finished at `position`.
    pub fn seeked(&self, position: Duration) {
        self.send(BackendEvent::Seeked(position));
    }
}

struct DefaultBackend(Rc<dyn Backend>);

impl Global for DefaultBackend {}

/// Sets the backend that [`VideoPlayer::new`](crate::VideoPlayer::new) uses.
/// Players that already exist keep the backend they were created with.
pub fn set_default_backend(backend: Rc<dyn Backend>, cx: &mut App) {
    cx.set_global(DefaultBackend(backend));
}

/// The backend new players use, if one has been set.
pub fn default_backend(cx: &App) -> Option<Rc<dyn Backend>> {
    cx.try_global::<DefaultBackend>()
        .map(|backend| backend.0.clone())
}
