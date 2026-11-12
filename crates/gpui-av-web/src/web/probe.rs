//! Asking for a URL again, to learn why a media element could not play it.

use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use crate::errors::Probe;

/// Requests the first byte of `url` and reports what the server said.
pub(crate) async fn probe(url: &str) -> Probe {
    match request(url, web_sys::RequestMode::Cors, true).await {
        Some(response) => Probe::Status(response.status()),
        // Either no answer, or an answer this page may not read. A request
        // that asks to read nothing tells the two apart.
        None => match request(url, web_sys::RequestMode::NoCors, false).await {
            Some(_) => Probe::Hidden,
            None => Probe::Unreachable,
        },
    }
}

async fn request(url: &str, mode: web_sys::RequestMode, range: bool) -> Option<web_sys::Response> {
    let window = web_sys::window()?;
    // The answer's headers are all that is wanted; the body is cancelled.
    let abort = web_sys::AbortController::new().ok()?;
    let init = web_sys::RequestInit::new();
    init.set_mode(mode);
    init.set_signal(Some(&abort.signal()));
    if range {
        let headers = web_sys::Headers::new().ok()?;
        headers.set("Range", "bytes=0-0").ok()?;
        init.set_headers(&headers);
    }
    let response = JsFuture::from(window.fetch_with_str_and_init(url, &init))
        .await
        .ok()?
        .dyn_into::<web_sys::Response>()
        .ok()?;
    abort.abort();
    Some(response)
}
