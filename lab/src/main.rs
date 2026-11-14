//! gpui-av-lab: the playground and benchmark app for gpui-av.
//!
//! ```sh
//! cargo run -p gpui-av-lab -- clips/                      # a folder
//! cargo run -p gpui-av-lab -- --muted a.mp4 b.mov         # files
//! cargo run -p gpui-av-lab -- --header "Authorization: Bearer token" \
//!     https://example.test/video.mp4                      # a URL with headers
//!
//! # Play the corpus without a person and write numbers as JSON:
//! cargo run --release -p gpui-av-lab -- --bench corpus/tier-a.json --clips clips --out bench.json
//! # The same corpus over HTTP:
//! cargo run --release -p gpui-av-lab -- --bench corpus/tier-a.json \
//!     --base-url http://127.0.0.1:8000/ --header "Authorization: Bearer token"
//! # Loop one clip for ten minutes and sample memory and CPU:
//! cargo run --release -p gpui-av-lab -- --soak 600 clips/h264-1920x1080-60fps.mp4
//! ```
//!
//! With no arguments it plays the built-in test patterns. `--bench` and
//! `--soak` always play muted.

mod backends;
mod bench;
mod clips;
#[cfg_attr(target_family = "wasm", path = "entry_web.rs")]
#[cfg_attr(not(target_family = "wasm"), path = "entry_native.rs")]
mod entry;
mod manifest;
mod metrics;
mod test_pattern;
mod view;

use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    App, AppContext, AsyncApp, Bounds, Entity, TitlebarOptions, WindowBounds, WindowOptions, px,
    size,
};
use gpui_av::{Backend, Source};

use crate::bench::Job;
use crate::clips::Clip;
use crate::manifest::Manifest;
use crate::view::{LabView, Settings};

const USAGE: &str = "\
usage: gpui-av-lab [options] [file, folder or URL]...

options:
  --header \"Name: value\"  a request header for every URL (repeatable)
  --backend <name>         start on (or bench) this backend; `all` benches each
  --muted                  start muted
  --loop                   loop every clip
  --help                   show this text

without a person (always muted; the result is JSON):
  --bench <manifest.json>  play every clip in the manifest and check it
  --clips <folder>         where the manifest's files are (default: clips)
  --base-url <url>         play the manifest's files from this URL instead
  --play-seconds <n>       how long to measure each clip (default: 5)
  --soak <seconds>         loop the first clip given and sample memory and CPU
  --out <file>             write the JSON here instead of standard output";

/// What the command line asked for.
#[derive(Debug, PartialEq)]
struct Options {
    targets: Vec<String>,
    headers: Vec<(String, String)>,
    backend: Option<String>,
    muted: bool,
    looping: bool,
    bench: Option<String>,
    clips: PathBuf,
    base_url: Option<String>,
    play_seconds: f64,
    soak: Option<f64>,
    out: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            targets: Vec::new(),
            headers: Vec::new(),
            backend: None,
            muted: false,
            looping: false,
            bench: None,
            clips: PathBuf::from("clips"),
            base_url: None,
            play_seconds: 5.0,
            soak: None,
            out: None,
        }
    }
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    fn seconds(option: &str, value: Option<String>) -> Result<f64, String> {
        value
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
            .ok_or(format!("{option} needs a number of seconds"))
    }
    let mut options = Options::default();
    while let Some(arg) = args.next() {
        let mut value = |what: &str| args.next().ok_or(format!("{arg} needs {what}"));
        match arg.as_str() {
            "--header" => {
                let header = value("\"Name: value\"")?;
                let (name, value) = header
                    .split_once(':')
                    .ok_or("a header looks like \"Name: value\"")?;
                options
                    .headers
                    .push((name.trim().to_string(), value.trim().to_string()));
            }
            "--backend" => options.backend = Some(value("a name")?),
            "--muted" => options.muted = true,
            "--loop" => options.looping = true,
            "--bench" => options.bench = Some(value("a manifest file")?),
            "--clips" => options.clips = value("a folder")?.into(),
            "--base-url" => options.base_url = Some(value("a URL")?),
            "--out" => options.out = Some(value("a file")?.into()),
            "--play-seconds" => options.play_seconds = seconds(&arg, args.next())?,
            "--soak" => options.soak = Some(seconds(&arg, args.next())?),
            "--help" | "-h" => return Err(USAGE.to_string()),
            option if option.starts_with("--") => {
                return Err(format!("unknown option {option}\n\n{USAGE}"));
            }
            _ => options.targets.push(arg),
        }
    }
    if options.bench.is_some() && options.soak.is_some() {
        return Err("--bench and --soak are separate runs".to_string());
    }
    Ok(options)
}

/// The manifest's clips as sources: files in the clips folder, or URLs
/// under the base URL with the request headers.
fn manifest_clips(manifest: &Manifest, options: &Options) -> Vec<Clip> {
    manifest
        .clips
        .iter()
        .map(|entry| {
            let source = match &options.base_url {
                Some(base) => {
                    let url = format!("{}/{}", base.trim_end_matches('/'), entry.file);
                    options
                        .headers
                        .iter()
                        .fold(Source::url(url), |source, (name, value)| {
                            source.header(name, value)
                        })
                }
                None => Source::file(options.clips.join(&entry.file)),
            };
            Clip {
                label: entry.file.clone(),
                source,
            }
        })
        .collect()
}

/// What the lab does once its window is open.
enum Mode {
    Interactive,
    Bench {
        jobs: Vec<Job>,
        backends: Vec<usize>,
    },
    Soak {
        job: Box<Job>,
        backend: usize,
        seconds: f64,
    },
}

/// Everything decided before the app starts.
struct Plan {
    backends: Vec<Rc<dyn Backend>>,
    clips: Vec<Clip>,
    settings: Settings,
    mode: Mode,
    play_seconds: f64,
    out: Option<PathBuf>,
}

/// Decides what to run. `manifest` is the text of the `--bench` manifest,
/// which the caller reads its own way (a file, or a fetch in a browser).
fn plan(options: Options, manifest: Option<&str>) -> Result<Plan, String> {
    let mut backends = backends::all();
    let chosen = match options.backend.as_deref() {
        None | Some("all") => None,
        Some(name) => Some(
            backends
                .iter()
                .position(|backend| backend.name() == name)
                .ok_or_else(|| {
                    let names: Vec<_> = backends.iter().map(|backend| backend.name()).collect();
                    format!(
                        "no backend named {name}; this build has: {}",
                        names.join(", ")
                    )
                })?,
        ),
    };
    let unattended = options.bench.is_some() || options.soak.is_some();
    let settings = Settings {
        muted: options.muted || unattended,
        looping: options.looping,
        ..Settings::default()
    };

    let (clips, mode) = if options.bench.is_some() {
        let manifest = Manifest::parse(manifest.ok_or("--bench needs its manifest")?)?;
        let clips = manifest_clips(&manifest, &options);
        let jobs = manifest
            .clips
            .into_iter()
            .enumerate()
            .map(|(clip, expected)| Job {
                clip,
                name: expected.file.clone(),
                expected: Some(expected),
            })
            .collect();
        // One backend unless `all` was asked for: the chosen one, or the
        // platform's own, which is first in the list.
        let backends = match (options.backend.as_deref(), chosen) {
            (Some("all"), _) => (0..backends.len()).collect(),
            (_, chosen) => vec![chosen.unwrap_or(0)],
        };
        (clips, Mode::Bench { jobs, backends })
    } else {
        let mut clips = clips::from_targets(&options.targets, &options.headers)?;
        if let Some(seconds) = options.soak {
            let first = clips.first().ok_or("--soak needs a clip to loop")?;
            let job = Box::new(Job {
                clip: 0,
                name: first.label.clone(),
                expected: None,
            });
            let backend = chosen.unwrap_or(0);
            (
                clips,
                Mode::Soak {
                    job,
                    backend,
                    seconds,
                },
            )
        } else {
            clips.extend(clips::built_in());
            // Interactive: the chosen backend goes first, so it starts.
            if let Some(chosen) = chosen {
                backends.swap(0, chosen);
            }
            (clips, Mode::Interactive)
        }
    };
    Ok(Plan {
        backends,
        clips,
        settings,
        mode,
        play_seconds: options.play_seconds,
        out: options.out,
    })
}

/// Runs the unattended mode and hands its report to the platform's entry,
/// with whether everything passed.
async fn run_unattended(
    view: Entity<LabView>,
    mode: Mode,
    play_seconds: f64,
    out: Option<PathBuf>,
    cx: &mut AsyncApp,
) {
    let (json, passed) = match mode {
        Mode::Interactive => return,
        Mode::Bench { jobs, backends } => {
            let report = bench::run(&view, &jobs, &backends, play_seconds, cx).await;
            let mut summary = format!("bench: {} passed, {} failed", report.passed, report.failed);
            for result in report.results.iter().filter(|result| !result.pass) {
                summary += &format!(
                    "\n  {} on {}: {}",
                    result.clip,
                    result.backend,
                    result.problems.join("; ")
                );
            }
            entry::log(&summary);
            (serde_json::to_string_pretty(&report), report.failed == 0)
        }
        Mode::Soak {
            job,
            backend,
            seconds,
        } => match bench::soak(&view, &job, backend, seconds, cx).await {
            Some(report) => (
                serde_json::to_string_pretty(&report),
                report.final_state == "Playing",
            ),
            None => {
                entry::log("the window closed during the soak");
                return entry::finish(None, None, false);
            }
        },
    };
    entry::finish(json.ok(), out.as_deref(), passed);
}

/// Opens the lab's window and starts what the plan says.
fn launch(plan: Plan, cx: &mut App) {
    let Plan {
        backends,
        clips,
        settings,
        mode,
        play_seconds,
        out,
    } = plan;
    let bounds = Bounds::centered(None, size(px(1280.), px(720.)), cx);
    let unattended = !matches!(mode, Mode::Interactive);
    let window = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("gpui-av lab".into()),
                    ..TitlebarOptions::default()
                }),
                // GPUI holds a window it thinks is inactive to 30 fps, which
                // drops every other frame of 60 fps video. Video in a window
                // without focus is ordinary (and on X11 with no window
                // manager the window is never active).
                inactive_frame_interval: None,
                ..WindowOptions::default()
            },
            // Unattended runs choose their own clips; nothing autoplays.
            |_, cx| cx.new(|cx| LabView::new(backends, clips, settings, !unattended, cx)),
        )
        .expect("open the lab's window");
    cx.activate(true);
    if unattended && let Ok(view) = window.entity(cx) {
        cx.spawn(async move |cx| run_unattended(view, mode, play_seconds, out, cx).await)
            .detach();
    }
}

fn main() {
    entry::main();
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
                ..Options::default()
            }
        );
    }

    #[test]
    fn a_bad_option_is_an_error() {
        assert!(parse(args(&["--header"])).is_err());
        assert!(parse(args(&["--header", "no colon"])).is_err());
        assert!(parse(args(&["--play-seconds", "soon"])).is_err());
        assert!(parse(args(&["--soak", "-3"])).is_err());
        assert!(parse(args(&["--bench", "m.json", "--soak", "5"])).is_err());
        assert!(parse(args(&["--fast"])).unwrap_err().contains("usage"));
    }

    #[test]
    fn bench_options_are_read() {
        let options = parse(args(&[
            "--bench",
            "corpus/tier-a.json",
            "--clips",
            "/tmp/clips",
            "--play-seconds",
            "2.5",
            "--out",
            "bench.json",
        ]))
        .unwrap();
        assert_eq!(options.bench.as_deref(), Some("corpus/tier-a.json"));
        assert_eq!(options.clips, PathBuf::from("/tmp/clips"));
        assert_eq!(options.play_seconds, 2.5);
        assert_eq!(options.out, Some("bench.json".into()));
    }

    fn manifest() -> Manifest {
        Manifest::parse(r#"{"clips": [{"file": "a.mp4", "expect": "plays"}]}"#).unwrap()
    }

    #[test]
    fn manifest_clips_are_files_in_the_clips_folder() {
        let options = Options {
            clips: "/corpus".into(),
            headers: vec![("Authorization".into(), "t".into())],
            ..Options::default()
        };
        let clips = manifest_clips(&manifest(), &options);
        assert_eq!(clips[0].label, "a.mp4");
        assert_eq!(clips[0].source, Source::file("/corpus/a.mp4"));
    }

    #[test]
    fn with_a_base_url_manifest_clips_are_urls_with_the_headers() {
        let options = Options {
            base_url: Some("http://127.0.0.1:8000/media/".into()),
            headers: vec![("Authorization".into(), "t".into())],
            ..Options::default()
        };
        let clips = manifest_clips(&manifest(), &options);
        assert_eq!(
            clips[0].source,
            Source::url("http://127.0.0.1:8000/media/a.mp4").header("Authorization", "t")
        );
    }

    #[test]
    fn a_bench_runs_muted_on_the_first_backend_unless_told_otherwise() {
        let bench = |backend: Option<&str>| {
            let options = Options {
                bench: Some("manifest.json".into()),
                backend: backend.map(str::to_string),
                ..Options::default()
            };
            plan(
                options,
                Some(r#"{"clips": [{"file": "a.mp4", "expect": "plays"}]}"#),
            )
            .unwrap()
        };

        let default = bench(None);
        assert!(default.settings.muted);
        let Mode::Bench { jobs, backends } = &default.mode else {
            panic!("expected a bench");
        };
        assert_eq!((jobs.len(), backends.as_slice()), (1, [0].as_slice()));

        let Mode::Bench { backends, .. } = bench(Some("all")).mode else {
            panic!("expected a bench");
        };
        assert_eq!(backends.len(), backends::all().len());

        let pattern = bench(Some("test-pattern"));
        let Mode::Bench { backends, .. } = &pattern.mode else {
            panic!("expected a bench");
        };
        assert_eq!(pattern.backends[backends[0]].name(), "test-pattern");
    }

    #[test]
    fn an_unknown_backend_is_an_error_that_lists_the_real_ones() {
        let options = Options {
            backend: Some("vlc".into()),
            ..Options::default()
        };
        let error = plan(options, None).err().unwrap();
        assert!(error.contains("test-pattern"), "{error}");
    }
}
