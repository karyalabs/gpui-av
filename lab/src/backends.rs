//! The backends the lab can play with. This is the only place in the lab
//! that differs per platform.

use std::rc::Rc;

use gpui_av::{Backend, Fallback};

use crate::test_pattern::TestPatternBackend;

/// Every backend available on this platform: the platform's own first
/// (the bench's default), then, where there are several, all of them
/// tried in order as an app gets from `gpui_av_platform::init`, then the
/// test pattern, which works everywhere.
pub fn all() -> Vec<Rc<dyn Backend>> {
    let own = own();
    let mut all = own.clone();
    if own.len() > 1 {
        all.push(Rc::new(Fallback::new(own)));
    }
    all.push(Rc::new(TestPatternBackend));
    all
}

#[cfg(not(target_family = "wasm"))]
fn own() -> Vec<Rc<dyn Backend>> {
    gpui_av_platform::backends()
}

// The worker sends the request headers a `<video>` element cannot;
// scripts/web puts its file next to the page.
#[cfg(target_family = "wasm")]
fn own() -> Vec<Rc<dyn Backend>> {
    vec![Rc::new(gpui_av_web::WebBackend::with_service_worker(
        "gpui-av-sw.js",
    ))]
}
