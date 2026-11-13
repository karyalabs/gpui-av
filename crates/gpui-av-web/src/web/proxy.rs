//! The page's side of the service worker that sends request headers.
//!
//! A source with headers plays from a made-up URL under the worker's scope.
//! The worker asks this page what the real URL and headers are, and this
//! module answers.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gpui_av::{Error, ErrorKind};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use super::{describe, sleep};

/// The path segment that marks a made-up URL; the worker looks for it.
const MARKER: &str = "__gpui-av__";
/// How long to wait for a registered worker to take control of the page.
const CONTROL_WAIT_MS: i32 = 5000;

/// What a made-up URL stands for.
struct Registered {
    url: String,
    headers: Vec<(String, String)>,
}

type Sources = Rc<RefCell<HashMap<String, Registered>>>;

pub(crate) struct Proxy {
    /// Settles when the worker controls the page, or cannot.
    ready: js_sys::Promise,
    /// The worker's scope: the directory of its script.
    scope: String,
    sources: Sources,
    next_id: Cell<u64>,
    _listener: Option<Closure<dyn FnMut(web_sys::MessageEvent)>>,
}

impl Proxy {
    /// Registers the worker at `script_url` and starts answering it.
    pub fn start(script_url: &str) -> Rc<Self> {
        let page = web_sys::window()
            .and_then(|window| window.location().href().ok())
            .unwrap_or_default();
        let script = web_sys::Url::new_with_base(script_url, &page)
            .map_or_else(|_| script_url.to_string(), |url| url.href());
        let scope = web_sys::Url::new_with_base("./", &script)
            .map_or_else(|_| "/".to_string(), |url| url.href());
        let sources = Sources::default();
        let listener = container().map(|container| {
            let listener = answer_lookups(sources.clone());
            let _ = container
                .add_event_listener_with_callback("message", listener.as_ref().unchecked_ref());
            // Messages sent before the page finished loading are held until
            // this is called.
            let _ = js_sys::Reflect::get(&container, &"startMessages".into())
                .ok()
                .and_then(|start| start.dyn_into::<js_sys::Function>().ok())
                .map(|start| start.call0(&container));
            listener
        });
        Rc::new(Self {
            ready: future_to_promise(take_control(script)),
            scope,
            sources,
            next_id: Cell::new(0),
            _listener: listener,
        })
    }

    /// Waits until the worker controls the page.
    pub async fn ready(&self) -> Result<(), Error> {
        JsFuture::from(self.ready.clone())
            .await
            .map(|_| ())
            .map_err(|error| {
                Error::new(
                    ErrorKind::Unsupported,
                    format!(
                        "this source has request headers, and the service worker that sends \
                         them is not running: {}",
                        describe(&error)
                    ),
                )
            })
    }

    /// Makes up a URL that plays `url` with `headers`. Returns the source's
    /// id and the URL.
    pub fn add(&self, url: &str, headers: &[(String, String)]) -> (String, String) {
        let number = self.next_id.get();
        self.next_id.set(number + 1);
        // Unpredictable, so that one page cannot guess another's sources.
        let random = (js_sys::Math::random() * f64::from(u32::MAX)) as u32;
        let id = format!("{number}-{random:08x}");
        // The last segment keeps the media's file name, for the extension.
        let name = url
            .split(['?', '#'])
            .next()
            .and_then(|path| path.rsplit('/').next())
            .filter(|name| !name.is_empty())
            .unwrap_or("media");
        let made_up = format!("{}{MARKER}/{id}/{name}", self.scope);
        self.sources.borrow_mut().insert(
            id.clone(),
            Registered {
                url: url.to_string(),
                headers: headers.to_vec(),
            },
        );
        (id, made_up)
    }

    /// Forgets a source, here and in the worker.
    pub fn remove(&self, id: &str) {
        self.sources.borrow_mut().remove(id);
        let Some(worker) = container().and_then(|container| container.controller()) else {
            return;
        };
        let message = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&message, &"gpuiAv".into(), &"forget".into());
        let _ = js_sys::Reflect::set(&message, &"id".into(), &id.into());
        let _ = worker.post_message(&message);
    }
}

/// `navigator.serviceWorker`, where the browser has it (a secure context).
fn container() -> Option<web_sys::ServiceWorkerContainer> {
    let navigator = web_sys::window()?.navigator();
    js_sys::Reflect::get(&navigator, &"serviceWorker".into())
        .ok()
        .filter(|container| !container.is_undefined() && !container.is_null())
        .map(JsCast::unchecked_into)
}

/// Registers the worker and waits for it to control this page.
async fn take_control(script: String) -> Result<JsValue, JsValue> {
    let container =
        container().ok_or("this page has no service workers (it needs a secure context)")?;
    JsFuture::from(container.register(&script)).await?;
    JsFuture::from(container.ready()?).await?;
    // The worker claims the page when it activates, which can land a moment
    // after `ready`. A page loaded with a forced reload is never claimed.
    let mut waited = 0;
    while container.controller().is_none() {
        if waited >= CONTROL_WAIT_MS {
            return Err("the service worker did not take control of the page \
                        (a forced reload bypasses it; reload normally)"
                .into());
        }
        sleep(50).await;
        waited += 50;
    }
    Ok(JsValue::UNDEFINED)
}

/// Answers the worker's `lookup` messages on the port they carry.
fn answer_lookups(sources: Sources) -> Closure<dyn FnMut(web_sys::MessageEvent)> {
    Closure::new(move |event: web_sys::MessageEvent| {
        let data = event.data();
        let field = |name: &str| {
            js_sys::Reflect::get(&data, &name.into())
                .ok()
                .and_then(|value| value.as_string())
        };
        if field("gpuiAv").as_deref() != Some("lookup") {
            return;
        }
        let Some(port) = event.ports().get(0).dyn_into::<web_sys::MessagePort>().ok() else {
            return;
        };
        let sources = sources.borrow();
        let reply = match field("id").and_then(|id| sources.get(&id)) {
            Some(source) => {
                let headers = js_sys::Array::new();
                for (name, value) in &source.headers {
                    headers.push(&js_sys::Array::of2(&name.into(), &value.into()));
                }
                let reply = js_sys::Object::new();
                let _ = js_sys::Reflect::set(&reply, &"url".into(), &source.url.as_str().into());
                let _ = js_sys::Reflect::set(&reply, &"headers".into(), &headers);
                reply.into()
            }
            None => JsValue::NULL,
        };
        let _ = port.post_message(&reply);
    })
}
