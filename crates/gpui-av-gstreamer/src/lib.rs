//! The GStreamer backend for gpui-av, the default on Linux.
//!
//! It plays media with GStreamer's `playbin`, which picks the demuxers and
//! decoders installed on the machine (hardware ones included), and draws
//! each frame as a GPUI image: the decoder's frame is converted to BGRA,
//! copied once into the image, and uploaded by GPUI. Audio plays through
//! GStreamer's own sink, whose clock the picture follows.
//!
//! GStreamer is loaded from the system when the first source opens, not
//! linked, so an app with this backend builds and starts on machines
//! without it; there, [`GStreamerBackend::is_available`] is false and every
//! source fails as `Unsupported`. `unsafe` and GStreamer types stay inside
//! this crate.
//!
//! ```ignore
//! gpui_av::set_default_backend(Rc::new(GStreamerBackend::new()), cx);
//! ```
//!
//! GStreamer runs on macOS and Windows too, so the crate builds there; it
//! is only tried on Linux so far. In a browser this crate is empty.

#[cfg(not(target_family = "wasm"))]
mod errors;
#[cfg(not(target_family = "wasm"))]
mod ffi;
#[cfg(not(target_family = "wasm"))]
mod pipeline;
#[cfg(not(target_family = "wasm"))]
mod session;
#[cfg(not(target_family = "wasm"))]
mod soup;

#[cfg(not(target_family = "wasm"))]
pub use native::GStreamerBackend;

#[cfg(not(target_family = "wasm"))]
mod native {
    use std::ffi::{CString, c_void};
    use std::path::Path;

    use gpui::App;
    use gpui_av::{Backend, Error, ErrorKind, EventSink, Location, Session, Source};

    use crate::ffi::{self, Api};
    use crate::pipeline::Pipeline;
    use crate::session::GstSession;
    use crate::soup::SoupFetch;
    use gpui_av_remux::HlsProxy;

    /// Plays media with GStreamer's `playbin` and draws it as GPUI images.
    ///
    /// Sources: file paths, and URLs GStreamer can open (`http` and
    /// `https` through its `souphttpsrc`). Request headers go with every
    /// request for the media; a URL whose source element cannot send them
    /// fails as `Unsupported` instead of loading without them.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct GStreamerBackend;

    impl GStreamerBackend {
        /// The backend. It holds no state; one is enough for an app.
        ///
        /// GStreamer starts loading on a background thread here, so the
        /// first media it opens does not wait for it on the main thread:
        /// on a fresh install, GStreamer scans every plugin before its
        /// first use, which can take a minute.
        pub fn new() -> Self {
            Self::warm_up();
            Self
        }

        fn warm_up() {
            static STARTED: std::sync::Once = std::sync::Once::new();
            STARTED.call_once(|| {
                let spawned = std::thread::Builder::new()
                    .name("gstreamer-load".into())
                    // Whether it loaded is for `open` to report.
                    .spawn(|| ffi::api().is_ok());
                if let Err(error) = spawned {
                    eprintln!(
                        "gpui-av-gstreamer: could not load GStreamer in the background: {error}"
                    );
                }
            });
        }

        /// Whether GStreamer 1.x is installed and starts. Loads it on the
        /// first call.
        pub fn is_available() -> bool {
            ffi::api().is_ok()
        }

        /// The version of the GStreamer in use, once it has loaded.
        pub fn gstreamer_version() -> Option<(u32, u32, u32)> {
            ffi::api().ok().map(|api| api.version)
        }
    }

    impl Backend for GStreamerBackend {
        fn name(&self) -> &'static str {
            "gstreamer"
        }

        fn presenter(&self) -> &'static str {
            if cfg!(feature = "karya-gpui") {
                "surface"
            } else {
                "image"
            }
        }

        fn open(
            &self,
            source: Source,
            events: EventSink,
            cx: &mut App,
        ) -> Result<Box<dyn Session>, Error> {
            let api = ffi::api().map_err(|error| Error::new(ErrorKind::Unsupported, error))?;
            let uri = match source.location() {
                Location::File(path) => file_uri(api, path)?,
                Location::Url(url) => url.clone(),
                _ => {
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        "the GStreamer backend takes file paths and URLs",
                    ));
                }
            };
            // With headers, GStreamer plays our loopback proxy, which sends
            // them to the source's origin only; souphttpsrc would send them
            // after a redirect to anywhere.
            let proxy = match source.location() {
                Location::Url(url) if !source.headers().is_empty() => {
                    match SoupFetch::new(url, source.headers()) {
                        Some(fetch) => Some(HlsProxy::start(url, fetch).map_err(|error| {
                            Error::new(
                                ErrorKind::Network,
                                format!("cannot start the local proxy for {url}: {error}"),
                            )
                        })?),
                        None => None,
                    }
                }
                _ => None,
            };
            let pipeline = match &proxy {
                Some(proxy) => Pipeline::new(api, proxy.url(), &[])?,
                None => Pipeline::new(api, &uri, source.headers())?,
            };
            Ok(Box::new(GstSession::new(pipeline, proxy, events, cx)))
        }
    }

    fn file_uri(api: &Api, path: &Path) -> Result<String, Error> {
        let path = path.to_str().ok_or_else(|| {
            Error::new(ErrorKind::Unsupported, "the file's path is not valid UTF-8")
        })?;
        let path = CString::new(path)
            .map_err(|_| Error::new(ErrorKind::NotFound, "the file's path has a NUL byte"))?;
        let mut error = std::ptr::null_mut();
        // SAFETY: a valid string and error slot; the result is ours to free.
        unsafe {
            let uri = (api.gst.gst_filename_to_uri)(path.as_ptr(), &mut error);
            if uri.is_null() {
                let message = ffi::take_error(&api.glib, error)
                    .map(|(_, _, message)| message)
                    .unwrap_or_default();
                return Err(Error::new(ErrorKind::NotFound, message));
            }
            let text = ffi::string(uri).unwrap_or_default();
            (api.glib.g_free)(uri as *mut c_void);
            Ok(text)
        }
    }
}
