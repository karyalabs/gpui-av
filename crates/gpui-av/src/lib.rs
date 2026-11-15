//! Audio and video playback for GPUI apps.
//!
//! gpui-av is a small typed API over the platform's own decoders. This crate
//! holds the API and no decoder: a [`VideoPlayer`] entity, the [`video()`]
//! element that draws it, and the public [`Backend`] trait that a decoder
//! stack implements. Backends live in their own crates (`gpui-av-avfoundation`,
//! `gpui-av-web`, `gpui-av-gstreamer`), and an app can bring its own. A
//! [`Fallback`] tries several backends in order, so media one platform
//! decoder will not open can still play through another.
//!
//! ```no_run
//! # use std::rc::Rc;
//! # use gpui::{App, AppContext, IntoElement, Styled};
//! # use gpui_av::{Backend, ObjectFit, Source, VideoPlayer, video};
//! # fn example(my_backend: Rc<dyn Backend>, url: &str, token: &str, cx: &mut App) {
//! // Once, at startup: pick the backend new players use (or call
//! // `gpui_av_platform::init(cx)` for the platform's own).
//! gpui_av::set_default_backend(my_backend, cx);
//!
//! // Create a player and start it.
//! let source = Source::url(url).header("Authorization", token);
//! let player = cx.new(|cx| VideoPlayer::new(source, cx));
//! player.update(cx, |player, _| player.play());
//!
//! // In render:
//! let element = video(&player).object_fit(ObjectFit::Contain).size_full();
//! # }
//! ```
//!
//! A whole app is `gpui-av-platform`'s `play` example.
//!
//! This crate has no `unsafe` and no platform types in its public items;
//! both stay inside backend crates.

#![forbid(unsafe_code)]

mod backend;
mod element;
mod error;
mod fallback;
mod media;
mod player;
mod source;
mod subtitles;

pub use backend::{
    Backend, BackendEvent, EventSink, Placement, Session, Stats, default_backend,
    set_default_backend,
};
pub use element::{Video, video};
pub use error::{Error, ErrorKind};
pub use fallback::Fallback;
pub use media::{
    ColorInfo, ColorMatrix, ColorRange, MediaInfo, Rotation, Track, TrackKind, TransferFunction,
};
pub use player::{PlaybackState, PlayerEvent, VideoPlayer};
pub use source::{Location, Source};
pub use subtitles::{Captions, Cue, SubtitleError, Subtitles, captions};

// The fit modes are GPUI's own, so `video()` reads like `img()`.
pub use gpui::ObjectFit;
