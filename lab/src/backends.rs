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
        Rc::new(TestPatternBackend),
    ]
}
