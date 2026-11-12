//! The macOS backend for gpui-av.
//!
//! It plays media with AVFoundation (`AVPlayer` and
//! `AVPlayerItemVideoOutput`) and draws each frame with GPUI's `surface`,
//! straight from the decoder's GPU memory. `unsafe` and Apple types stay
//! inside this crate.
//!
//! Not implemented yet: the backend arrives with experiment E1
//! (`docs/spec.md`, section 5.2).
