//! The backends the lab can play with. This is the only place in the lab
//! that differs per platform.

use std::rc::Rc;

use gpui_av::Backend;

use crate::test_pattern::TestPatternBackend;

/// Every backend available on this platform: the platform's own first, then
/// the test pattern, which works everywhere.
pub fn all() -> Vec<Rc<dyn Backend>> {
    vec![
        #[cfg(target_os = "macos")]
        Rc::new(gpui_av_avfoundation::AvFoundationBackend::new()),
        // The worker sends the request headers a `<video>` element cannot;
        // scripts/web puts its file next to the page.
        #[cfg(target_family = "wasm")]
        Rc::new(gpui_av_web::WebBackend::with_service_worker(
            "gpui-av-sw.js",
        )),
        Rc::new(TestPatternBackend),
    ]
}
