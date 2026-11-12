//! The macOS backend for gpui-av.
//!
//! It plays media with AVFoundation (`AVPlayer` and
//! `AVPlayerItemVideoOutput`) and draws each frame with GPUI's `surface`,
//! straight from the decoder's GPU memory. `unsafe` and Apple types stay
//! inside this crate.
//!
//! ```ignore
//! gpui_av::set_default_backend(Rc::new(AvFoundationBackend::new()), cx);
//! ```
//!
//! On other targets this crate is empty.

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::AvFoundationBackend;
