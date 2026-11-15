//! The backend: one `<video>` element per session.

mod mse;
mod probe;
mod proxy;
mod session;

use std::rc::Rc;

use gpui::App;
use gpui_av::{Backend, Error, ErrorKind, EventSink, Session, Source};
use wasm_bindgen::JsValue;

use self::proxy::Proxy;
use self::session::WebSession;

/// The web backend: plays a URL with the browser's own `<video>` element,
/// placed over the GPUI canvas.
///
/// A Matroska or WebM file the browser will not open (Safari and Firefox
/// with Matroska, Safari with most WebM) is remuxed to fragmented MP4 as it
/// plays and fed to the same element through Media Source Extensions, so
/// the browser's own decoders still play it. That needs a server that
/// takes range requests (and, for another origin, CORS that allows them).
#[derive(Default)]
pub struct WebBackend {
    proxy: Option<Rc<Proxy>>,
    remux_always: bool,
}

impl WebBackend {
    /// A backend that refuses sources with request headers.
    pub fn new() -> Self {
        Self::default()
    }

    /// A backend that also plays sources with request headers, through the
    /// service worker at `script_url` (relative to the page, or absolute).
    /// The script is [`SERVICE_WORKER_JS`](crate::SERVICE_WORKER_JS), and
    /// the page must be inside its scope: serve it from the page's
    /// directory. The worker is registered now; a source with headers
    /// waits for it, and fails with [`ErrorKind::Unsupported`] if the
    /// browser will not run it.
    pub fn with_service_worker(script_url: &str) -> Self {
        Self {
            proxy: Some(Proxy::start(script_url)),
            remux_always: false,
        }
    }

    /// Plays every Matroska and WebM file through Media Source Extensions,
    /// even where the browser would open it itself: one path in every
    /// browser, and a way to test that path in one that needs it less.
    /// Sniffing the file costs one small request before it plays.
    pub fn remux_matroska_always(mut self) -> Self {
        self.remux_always = true;
        self
    }
}

impl Backend for WebBackend {
    fn name(&self) -> &'static str {
        "web"
    }

    fn presenter(&self) -> &'static str {
        "overlay"
    }

    fn open(
        &self,
        source: Source,
        events: EventSink,
        _cx: &mut App,
    ) -> Result<Box<dyn Session>, Error> {
        let Some(url) = source.as_url() else {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "the web backend plays URLs; a page cannot open a file path",
            ));
        };
        let proxy = match (source.headers().is_empty(), &self.proxy) {
            (true, _) => None,
            (false, Some(proxy)) => Some(proxy.clone()),
            (false, None) => {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    "this source has request headers, which a <video> element cannot send; \
                     start the backend with WebBackend::with_service_worker to send them",
                ));
            }
        };
        let session = WebSession::open(url, source.headers(), proxy, self.remux_always, events)
            .map_err(|error| Error::new(ErrorKind::Other, describe(&error)))?;
        Ok(Box::new(session))
    }
}

/// A JavaScript error as text.
fn describe(error: &JsValue) -> String {
    js_sys::Reflect::get(error, &"message".into())
        .ok()
        .and_then(|message| message.as_string())
        .or_else(|| error.as_string())
        .unwrap_or_else(|| format!("{error:?}"))
}

/// A promise that settles after `milliseconds`.
async fn sleep(milliseconds: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(window) = web_sys::window() {
            let _ = window
                .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, milliseconds);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}
