//! The backend: one `<video>` element per session.

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
#[derive(Default)]
pub struct WebBackend {
    proxy: Option<Rc<Proxy>>,
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
        }
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
        let session = WebSession::open(url, source.headers(), proxy, events)
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
