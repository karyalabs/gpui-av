//! The lab's entry in a browser: options from the page's query string, and
//! reports handed to the page.
//!
//! The query string carries the command line: `?muted&backend=test-pattern`
//! is `--muted --backend test-pattern`, and each `clip=` is a clip to list.
//! Clip, manifest and base URLs may be relative to the page. An unattended
//! run leaves its JSON in `window.gpuiAvLab.report` (with `done` and
//! `passed` beside it); with `out=reports/<name>.json` it also POSTs the
//! JSON to that URL, or offers it as a download when no server takes it.

use std::path::Path;

use gpui::App;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

/// The list of clips `scripts/web serve` makes from the folder it serves:
/// a JSON array of file names, relative to itself.
const CLIP_LIST: &str = "clips/index.json";

/// A browser gives GPUI no system fonts, so the lab brings one: the family
/// GPUI's web platform falls back to.
const FONT: &[u8] = include_bytes!("../assets/IBMPlexSans-Regular.ttf");

/// Reads the query string and runs the lab.
pub fn main() {
    gpui_platform::web_init();
    let mut options = match crate::parse(arguments().into_iter()) {
        Ok(options) => options,
        Err(message) => return log(&message),
    };
    wasm_bindgen_futures::spawn_local(async move {
        let manifest = match &options.bench {
            Some(url) => match fetch_text(url).await {
                Ok(text) => Some(text),
                Err(message) => return log(&format!("{url}: {message}")),
            },
            None => None,
        };
        // With no clip named, list what the server offers, if anything.
        if options.targets.is_empty() && options.bench.is_none() {
            options.targets = served_clips().await;
        }
        match crate::plan(options, manifest.as_deref()) {
            Ok(plan) => {
                let app = gpui_platform::single_threaded_web();
                // The page owns the run loop; the app lives as long as it.
                std::mem::forget(app.run_embedded(move |cx: &mut App| {
                    cx.text_system()
                        .add_fonts(vec![std::borrow::Cow::Borrowed(FONT)])
                        .expect("load the lab's font");
                    crate::launch(plan, cx);
                }));
            }
            Err(message) => log(&message),
        }
    });
}

/// The URLs of the clips in [`CLIP_LIST`]; none when there is no list.
async fn served_clips() -> Vec<String> {
    let list = absolute(CLIP_LIST);
    let Ok(text) = fetch_text(&list).await else {
        return Vec::new();
    };
    let names: Vec<String> = serde_json::from_str(&text).unwrap_or_default();
    names
        .iter()
        .filter_map(|name| web_sys::Url::new_with_base(&encode(name), &list).ok())
        .map(|url| url.href())
        .collect()
}

/// A file name as a URL path segment.
fn encode(name: &str) -> String {
    js_sys::encode_uri_component(name).into()
}

/// `url` made absolute against the page's own.
fn absolute(url: &str) -> String {
    let page = web_sys::window()
        .and_then(|window| window.location().href().ok())
        .unwrap_or_default();
    web_sys::Url::new_with_base(url, &page).map_or_else(|_| url.to_string(), |url| url.href())
}

/// The query string as command-line arguments.
fn arguments() -> Vec<String> {
    let Some(window) = web_sys::window() else {
        return Vec::new();
    };
    let Some(params) = window
        .location()
        .search()
        .ok()
        .and_then(|search| web_sys::UrlSearchParams::new_with_str(&search).ok())
    else {
        return Vec::new();
    };
    let mut arguments = Vec::new();
    for entry in js_sys::Array::from(&params).iter() {
        let pair = js_sys::Array::from(&entry);
        let key = pair.get(0).as_string().unwrap_or_default();
        let value = pair.get(1).as_string().unwrap_or_default();
        match key.as_str() {
            "clip" => arguments.push(absolute(&value)),
            "bench" | "base-url" => arguments.extend([format!("--{key}"), absolute(&value)]),
            _ if value.is_empty() => arguments.push(format!("--{key}")),
            _ => arguments.extend([format!("--{key}"), value]),
        }
    }
    arguments
}

async fn fetch_text(url: &str) -> Result<String, String> {
    let describe = |error: JsValue| format!("{error:?}");
    let window = web_sys::window().ok_or("no window")?;
    let response: web_sys::Response = JsFuture::from(window.fetch_with_str(url))
        .await
        .map_err(describe)?
        .dyn_into()
        .map_err(describe)?;
    if !response.ok() {
        return Err(format!("HTTP {}", response.status()));
    }
    JsFuture::from(response.text().map_err(describe)?)
        .await
        .map_err(describe)?
        .as_string()
        .ok_or_else(|| "the response is not text".to_string())
}

/// A line for the person running the lab: the browser's console.
pub fn log(message: &str) {
    web_sys::console::log_1(&message.into());
}

/// Ends an unattended run: leaves the report on `window.gpuiAvLab`, and
/// with `out` also POSTs it there (a URL relative to the page, which
/// `scripts/serve.py` saves under `reports/`). When the server does not
/// take it, the report is offered as a download of that name instead.
pub fn finish(report: Option<String>, out: Option<&Path>, passed: bool) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let state = js_sys::Object::new();
    let set = |key: &str, value: JsValue| {
        let _ = js_sys::Reflect::set(&state, &key.into(), &value);
    };
    set("done", true.into());
    set("passed", (passed && report.is_some()).into());
    set(
        "report",
        report.as_deref().map_or(JsValue::NULL, JsValue::from),
    );
    let _ = js_sys::Reflect::set(&window, &"gpuiAvLab".into(), &state);

    let (Some(json), Some(out)) = (report, out) else {
        return;
    };
    let url = absolute(&out.to_string_lossy());
    let name = out
        .file_name()
        .map_or("gpui-av-lab.json".into(), |name| name.to_string_lossy())
        .into_owned();
    wasm_bindgen_futures::spawn_local(async move {
        let init = web_sys::RequestInit::new();
        init.set_method("POST");
        init.set_body(&JsValue::from_str(&json));
        let saved = JsFuture::from(window.fetch_with_str_and_init(&url, &init))
            .await
            .ok()
            .and_then(|response| response.dyn_into::<web_sys::Response>().ok())
            .is_some_and(|response| response.ok());
        if saved {
            log(&format!("the report was saved at {url}"));
        } else {
            let _ = download(&window, &name, &json);
        }
    });
}

fn download(window: &web_sys::Window, name: &str, text: &str) -> Result<(), JsValue> {
    let parts = js_sys::Array::of1(&text.into());
    let options = web_sys::BlobPropertyBag::new();
    options.set_type("application/json");
    let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts, &options)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob)?;
    let document = window.document().ok_or("no document")?;
    let link: web_sys::HtmlAnchorElement = document.create_element("a")?.dyn_into()?;
    link.set_href(&url);
    link.set_download(name);
    link.click();
    web_sys::Url::revoke_object_url(&url)
}
