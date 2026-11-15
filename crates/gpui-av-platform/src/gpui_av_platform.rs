//! The right gpui-av backends for each platform, in one call.
//!
//! ```ignore
//! // Once, at startup.
//! gpui_av_platform::init(cx);
//! // From then on.
//! let player = cx.new(|cx| VideoPlayer::new(Source::file(path), cx));
//! ```
//!
//! [`init`] sets a [`Fallback`] over [`backends`] as gpui-av's default
//! backend, so media the first backend refuses as unsupported is tried on
//! the next:
//!
//! | Target | Backends, in order |
//! |---|---|
//! | macOS | AVFoundation, then GStreamer where it is installed (for MKV, WebM and other media AVFoundation will not open) |
//! | Windows | Media Foundation, then GStreamer where it is installed |
//! | Linux and other desktops | GStreamer |
//! | wasm32 | the browser's `<video>` element, without request headers (see below) |
//!
//! Each backend is a cargo feature, on by default; turn one off to leave
//! it out. An app that wants another order, or backends of its own, builds
//! its own [`Fallback`] and passes it to [`gpui_av::set_default_backend`].
//!
//! In a browser a `<video>` element cannot send request headers, so
//! sources with headers fail as `Unsupported` here. An app that needs them
//! serves `gpui_av_web::SERVICE_WORKER_JS` next to its page and sets
//! `gpui_av_web::WebBackend::with_service_worker(url)` as the default
//! itself.

#![forbid(unsafe_code)]

use std::rc::Rc;

use gpui::App;
use gpui_av::{Backend, Fallback};

/// This platform's backends, the first choice first. Empty when every
/// backend that runs here is turned off.
// Each push is behind its own `cfg`, so `vec![..]` cannot list them.
#[allow(clippy::vec_init_then_push)]
pub fn backends() -> Vec<Rc<dyn Backend>> {
    let mut backends: Vec<Rc<dyn Backend>> = Vec::new();
    #[cfg(all(feature = "avfoundation", target_os = "macos"))]
    backends.push(Rc::new(gpui_av_avfoundation::AvFoundationBackend::new()));
    #[cfg(all(feature = "mediafoundation", windows))]
    backends.push(Rc::new(
        gpui_av_mediafoundation::MediaFoundationBackend::new(),
    ));
    #[cfg(all(feature = "gstreamer", not(target_family = "wasm")))]
    backends.push(Rc::new(gpui_av_gstreamer::GStreamerBackend::new()));
    #[cfg(all(feature = "web", target_family = "wasm"))]
    backends.push(Rc::new(gpui_av_web::WebBackend::new()));
    backends
}

/// [`backends`], tried in order.
pub fn backend() -> Fallback {
    Fallback::new(backends())
}

/// Makes [`backend`] the backend new players use.
pub fn init(cx: &mut App) {
    gpui_av::set_default_backend(Rc::new(backend()), cx);
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_platform_with_a_default_feature_set_has_a_backend() {
        let names: Vec<_> = super::backends()
            .iter()
            .map(|backend| backend.name())
            .collect();
        #[cfg(target_os = "macos")]
        assert_eq!(names, ["avfoundation", "gstreamer"]);
        #[cfg(windows)]
        assert_eq!(names, ["mediafoundation", "gstreamer"]);
        #[cfg(not(any(target_os = "macos", windows, target_family = "wasm")))]
        assert_eq!(names, ["gstreamer"]);
    }
}
