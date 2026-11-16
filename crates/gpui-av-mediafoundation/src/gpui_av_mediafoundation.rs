//! The Windows backend for gpui-av.
//!
//! It plays media with Media Foundation's media engine (`IMFMediaEngine`,
//! the engine behind the browser's `<video>` on Windows) in frame-server
//! mode: the engine decodes (in hardware where the machine can), plays the
//! sound and keeps the clock, and each frame due is copied into a GPUI
//! image and drawn with `paint_image`. `unsafe` and Windows types stay
//! inside this crate.
//!
//! ```ignore
//! gpui_av::set_default_backend(Rc::new(MediaFoundationBackend::new()), cx);
//! ```
//!
//! URLs with request headers (a media server's token) are read through a
//! byte stream of ours, with WinHTTP range requests, since the engine's own
//! loading cannot send headers. The headers go only to the URL's origin,
//! never to another one a redirect leads to.
//!
//! On other targets this crate is empty.

#[cfg(windows)]
mod engine;
#[cfg(windows)]
mod http;
#[cfg(any(windows, test))]
mod url;

#[cfg(windows)]
pub use engine::MediaFoundationBackend;
