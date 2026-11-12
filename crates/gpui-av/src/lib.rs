//! Audio and video playback for GPUI apps.
//!
//! gpui-av is a small typed API over the platform's own decoders. This crate
//! holds the API and no decoder: a [`VideoPlayer`] entity, the [`video()`]
//! element that draws it, and the public [`Backend`] trait that a decoder
//! stack implements. Backends live in their own crates (`gpui-av-avfoundation`,
//! `gpui-av-web`), and an app can bring its own.
//!
//! ```ignore
//! // Once, at startup: pick the backend new players use.
//! gpui_av::set_default_backend(my_backend, cx);
//!
//! // Create a player and start it.
//! let source = Source::url(url).header("Authorization", token);
//! let player = cx.new(|cx| VideoPlayer::new(source, cx));
//! player.update(cx, |player, _| player.play());
//!
//! // In render:
//! video(&player).object_fit(ObjectFit::Contain).size_full()
//! ```
//!
//! This crate has no `unsafe` and no platform types in its public items;
//! both stay inside backend crates.

#![forbid(unsafe_code)]

mod backend;
mod element;
mod error;
mod media;
mod player;
mod source;

pub use backend::{
    Backend, BackendEvent, EventSink, Placement, Session, Stats, default_backend,
    set_default_backend,
};
pub use element::{Video, video};
pub use error::{Error, ErrorKind};
pub use media::{ColorInfo, ColorMatrix, ColorRange, MediaInfo, Rotation};
pub use player::{PlaybackState, PlayerEvent, VideoPlayer};
pub use source::{Location, Source};

// The fit modes are GPUI's own, so `video()` reads like `img()`.
pub use gpui::ObjectFit;
