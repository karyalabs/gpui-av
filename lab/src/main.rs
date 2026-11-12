//! gpui-av-lab: the playground for gpui-av.
//!
//! ```sh
//! cargo run -p gpui-av-lab -- clips/                      # a folder
//! cargo run -p gpui-av-lab -- --muted a.mp4 b.mov         # files
//! cargo run -p gpui-av-lab -- --header "Authorization: Bearer token" \
//!     https://example.test/video.mp4                      # a URL with headers
//! ```
//!
//! With no arguments it plays the built-in test patterns.

mod backends;
mod clips;
mod test_pattern;
mod view;

use gpui::{
    App, AppContext, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions, actions, px,
    size,
};

use crate::view::{LabView, Settings};

actions!(
    lab,
    [
        /// Ends the app (Cmd-Q).
        Quit
    ]
);

const USAGE: &str = "\
usage: gpui-av-lab [options] [file, folder or URL]...

options:
  --header \"Name: value\"  a request header for every URL (repeatable)
  --backend <name>         start on this backend (see the list in the window)
  --muted                  start muted
  --loop                   loop every clip
  --help                   show this text";

/// What the command line asked for.
#[derive(Debug, Default, PartialEq)]
struct Options {
    targets: Vec<String>,
    headers: Vec<(String, String)>,
    backend: Option<String>,
    muted: bool,
    looping: bool,
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--header" => {
                let header = args.next().ok_or("--header needs \"Name: value\"")?;
                let (name, value) = header
                    .split_once(':')
                    .ok_or("a header looks like \"Name: value\"")?;
                options
                    .headers
                    .push((name.trim().to_string(), value.trim().to_string()));
            }
            "--backend" => {
                options.backend = Some(args.next().ok_or("--backend needs a name")?);
            }
            "--muted" => options.muted = true,
            "--loop" => options.looping = true,
            "--help" | "-h" => return Err(USAGE.to_string()),
            option if option.starts_with("--") => {
                return Err(format!("unknown option {option}\n\n{USAGE}"));
            }
            _ => options.targets.push(arg),
        }
    }
    Ok(options)
}

fn main() {
    let options = match parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let mut clips = match clips::from_targets(&options.targets, &options.headers) {
        Ok(clips) => clips,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    clips.extend(clips::built_in());

    let mut backends = backends::all();
    if let Some(name) = &options.backend {
        let Some(index) = backends.iter().position(|backend| backend.name() == name) else {
            let names: Vec<_> = backends.iter().map(|backend| backend.name()).collect();
            eprintln!(
                "no backend named {name}; this build has: {}",
                names.join(", ")
            );
            std::process::exit(2);
        };
        backends.swap(0, index);
    }
    let settings = Settings {
        muted: options.muted,
        looping: options.looping,
        ..Settings::default()
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
        let bounds = Bounds::centered(None, size(px(1280.), px(720.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("gpui-av lab".into()),
                    ..TitlebarOptions::default()
                }),
                ..WindowOptions::default()
            },
            |_, cx| cx.new(|cx| LabView::new(backends, clips, settings, cx)),
        )
        .expect("open the lab's window");
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(args: &[&str]) -> impl Iterator<Item = String> {
        args.iter()
            .map(|arg| arg.to_string())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn options_and_targets_are_told_apart() {
        let options = parse(args(&[
            "--muted",
            "clips",
            "--header",
            "Authorization: Bearer a:b",
            "--backend",
            "test-pattern",
            "https://example.test/a.mp4",
            "--loop",
        ]))
        .unwrap();
        assert_eq!(
            options,
            Options {
                targets: vec!["clips".into(), "https://example.test/a.mp4".into()],
                headers: vec![("Authorization".into(), "Bearer a:b".into())],
                backend: Some("test-pattern".into()),
                muted: true,
                looping: true,
            }
        );
    }

    #[test]
    fn a_bad_option_is_an_error() {
        assert!(parse(args(&["--header"])).is_err());
        assert!(parse(args(&["--header", "no colon"])).is_err());
        assert!(parse(args(&["--fast"])).unwrap_err().contains("usage"));
    }
}
