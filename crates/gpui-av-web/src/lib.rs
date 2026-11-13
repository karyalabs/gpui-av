//! The web backend for gpui-av.
//!
//! It plays media with an `HTMLVideoElement` that it keeps placed over the
//! bounds of the `video()` element, on top of GPUI's canvas. The browser
//! decodes, keeps audio and video in step and presents the frames; GPUI
//! content cannot draw over the picture. `unsafe` and browser types stay
//! inside this crate.
//!
//! ```ignore
//! gpui_av::set_default_backend(Rc::new(WebBackend::new()), cx);
//! ```
//!
//! # Request headers
//!
//! A `<video>` element cannot send request headers. A source with headers
//! is refused with [`ErrorKind::Unsupported`](gpui_av::ErrorKind) unless
//! the backend was started with `WebBackend::with_service_worker`, in which
//! case a service worker ([`SERVICE_WORKER_JS`]) fetches the media with the
//! headers on the element's behalf. Headers are never dropped silently.
//!
//! # What a browser does not tell
//!
//! The colour range and matrix, the rotation tag (the browser applies it;
//! the reported size is the rotated one) and, in some browsers, whether
//! there is an audio track.
//!
//! On targets other than `wasm32` this crate holds only
//! [`SERVICE_WORKER_JS`].

/// The source of the service worker that sends request headers for the
/// backend. An app serves this text as a JavaScript file next to its page
/// and passes the file's URL to `WebBackend::with_service_worker`.
pub const SERVICE_WORKER_JS: &str = include_str!("../assets/gpui-av-sw.js");

#[cfg(any(target_family = "wasm", test))]
mod errors;
#[cfg(any(target_family = "wasm", test))]
mod layout;
#[cfg(target_family = "wasm")]
mod web;

#[cfg(target_family = "wasm")]
pub use web::WebBackend;
