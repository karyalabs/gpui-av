//! The lab's entry on a desktop: options from the command line, reports to
//! a file or standard output, and an exit status.

use std::path::Path;

use gpui::{App, KeyBinding, actions};

actions!(
    lab,
    [
        /// Ends the app (Cmd-Q).
        Quit
    ]
);

/// Reads the command line and runs the lab.
pub fn main() {
    let planned = crate::parse(std::env::args().skip(1)).and_then(|options| {
        let manifest = match &options.bench {
            Some(path) => {
                Some(std::fs::read_to_string(path).map_err(|error| format!("{path}: {error}"))?)
            }
            None => None,
        };
        crate::plan(options, manifest.as_deref())
    });
    let plan = match planned {
        Ok(plan) => plan,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    gpui_platform::application().run(move |cx: &mut App| {
        // The lab is one window: closing it, or Cmd-Q, ends the app.
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        crate::launch(plan, cx);
    });
}

/// A line for the person running the lab: standard error, so a report on
/// standard output stays clean JSON.
pub fn log(message: &str) {
    eprintln!("{message}");
}

/// Ends an unattended run: writes the report to `out` or standard output
/// and exits with 0 when everything passed, 1 when not.
pub fn finish(report: Option<String>, out: Option<&Path>, passed: bool) {
    let written = match (report, out) {
        (Some(json), Some(path)) => std::fs::write(path, json + "\n")
            .inspect_err(|error| eprintln!("{}: {error}", path.display()))
            .is_ok(),
        (Some(json), None) => {
            println!("{json}");
            true
        }
        (None, _) => false,
    };
    std::process::exit(if written && passed { 0 } else { 1 });
}
