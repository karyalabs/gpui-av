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
//! Sources with request headers are refused as `Unsupported` for now, so a
//! [`gpui_av::Fallback`] passes them on to the next backend.
//!
//! On other targets this crate is empty.

#[cfg(windows)]
mod engine;

#[cfg(windows)]
pub use engine::MediaFoundationBackend;
