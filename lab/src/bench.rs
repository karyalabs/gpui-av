//! `--bench` and `--soak`: play clips without a person and report numbers.
//!
//! The bench drives the same [`LabView`] a person would, in a real window,
//! so what it measures is what the lab shows. It only decides what to play
//! and when, and reads the player through the public gpui-av API.

use std::time::Duration;

use gpui::{AsyncApp, Entity};
use gpui_av::{ColorMatrix, ColorRange, MediaInfo, PlaybackState, Stats};
use serde::Serialize;
use web_time::{Instant, SystemTime, UNIX_EPOCH};

use crate::manifest::{Checks, Expected, Outcome};
use crate::metrics;
use crate::view::LabView;

/// How often the bench looks at the player.
const POLL: Duration = Duration::from_millis(4);
/// How long a clip may take to show its first frame (or fail).
const START_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a clip that must fail may take to do so.
const FAIL_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a seek may take.
const SEEK_TIMEOUT: Duration = Duration::from_secs(5);
/// The bench stops this long before the end of a clip, so the seek that
/// follows starts from a playing clip, not an ended one.
const END_MARGIN: f64 = 0.5;
/// The soak samples this often, and ignores growth before its first
/// sample: decoders and caches fill up in the first seconds.
const SOAK_SAMPLE: Duration = Duration::from_secs(5);

/// One clip to bench: its index in the lab's clip list, and what the
/// manifest expects of it, if it came from one.
#[derive(Clone, Debug)]
pub struct Job {
    /// Index into the [`LabView`]'s clips.
    pub clip: usize,
    /// The name to report.
    pub name: String,
    /// The manifest entry, when there is one.
    pub expected: Option<Expected>,
}

/// The bench's whole output.
#[derive(Debug, Serialize)]
pub struct Report {
    /// The operating system, as Rust names it (`macos`, `linux`).
    pub os: &'static str,
    /// The CPU architecture (`aarch64`, `x86_64`, `wasm32`).
    pub arch: &'static str,
    /// When the bench started, in seconds since 1970.
    pub started_unix: u64,
    /// How many results passed.
    pub passed: usize,
    /// How many results did not.
    pub failed: usize,
    /// One entry per clip and backend.
    pub results: Vec<ClipResult>,
}

/// What happened to one clip on one backend.
#[derive(Debug, Serialize)]
pub struct ClipResult {
    /// The clip's name.
    pub clip: String,
    /// The backend's name.
    pub backend: &'static str,
    /// How the backend presents frames.
    pub presenter: &'static str,
    /// `played`, `failed` or `timed_out`.
    pub outcome: &'static str,
    /// The error, when the clip failed.
    pub error: Option<ErrorReport>,
    /// Whether the outcome and the reported properties are what the
    /// manifest expects (or, with no manifest, whether the clip played).
    pub pass: bool,
    /// Why `pass` is false, in words.
    pub problems: Vec<String>,
    /// From creating the player to its first frame on screen.
    pub time_to_first_frame_ms: Option<f64>,
    /// From asking for a seek (to the middle, or a quarter in) to the backend reporting the
    /// new frame ready.
    pub seek_ms: Option<f64>,
    /// How long the clip was played and measured for.
    pub played_seconds: f64,
    /// Frames shown during the measured time.
    pub frames_presented: u64,
    /// Frames dropped during the measured time.
    pub frames_dropped: u64,
    /// Dropped frames as a share of all frames due.
    pub dropped_percent: Option<f64>,
    /// Frames shown per second during the measured time.
    pub video_fps: Option<f64>,
    /// The largest gap between a frame's timestamp and the clock when it
    /// was shown; `None` when the backend does not measure it.
    pub max_drift_ms: Option<f64>,
    /// CPU time used by the process while this clip was open, as a share
    /// of one core; `None` where it cannot be read.
    pub cpu_percent: Option<f64>,
    /// The process's resident memory when the clip finished.
    pub memory_mb: Option<f64>,
    /// What the backend reported about the media.
    pub reported: Reported,
    /// What the manifest expects.
    pub expected: Option<Expected>,
    /// How the two compare.
    pub checks: Option<Checks>,
}

/// An error, as the bench reports it.
#[derive(Debug, Serialize)]
pub struct ErrorReport {
    /// The `ErrorKind`, by name.
    pub kind: String,
    /// The backend's message.
    pub message: String,
}

/// A [`MediaInfo`] in the manifest's terms.
#[derive(Debug, Default, Serialize)]
pub struct Reported {
    /// Length in seconds.
    pub duration: Option<f64>,
    /// Width as shown.
    pub width: Option<u32>,
    /// Height as shown.
    pub height: Option<u32>,
    /// Rotation tag in degrees clockwise.
    pub rotation: u32,
    /// Whether there is a video track.
    pub has_video: bool,
    /// Whether there is an audio track.
    pub has_audio: bool,
    /// `limited`, `full`, or `None` when unknown.
    pub color_range: Option<&'static str>,
    /// `bt601`, `bt709`, `bt2020`, or `None` when unknown.
    pub color_matrix: Option<&'static str>,
}

impl From<&MediaInfo> for Reported {
    fn from(info: &MediaInfo) -> Self {
        Self {
            duration: info.duration.map(|duration| duration.as_secs_f64()),
            width: info.natural_size.map(|size| size.width),
            height: info.natural_size.map(|size| size.height),
            rotation: info.rotation.degrees(),
            has_video: info.has_video,
            has_audio: info.has_audio,
            color_range: info.color.range.map(|range| match range {
                ColorRange::Limited => "limited",
                ColorRange::Full => "full",
            }),
            color_matrix: info.color.matrix.map(|matrix| match matrix {
                ColorMatrix::Bt601 => "bt601",
                ColorMatrix::Bt709 => "bt709",
                ColorMatrix::Bt2020 => "bt2020",
                _ => "other",
            }),
        }
    }
}

/// The player at one moment.
#[derive(Clone, Debug)]
struct Snapshot {
    state: PlaybackState,
    stats: Stats,
    info: MediaInfo,
    last_seek: Option<Duration>,
}

impl Snapshot {
    fn failed(&self) -> bool {
        matches!(self.state, PlaybackState::Failed(_))
    }

    /// Whether playback has visibly begun: a frame is on screen, or, for
    /// media without a picture, the backend says it is playing.
    fn started(&self) -> bool {
        self.stats.frames_presented > 0
            || (self.state == PlaybackState::Playing && self.info.has_audio && !self.info.has_video)
    }
}

fn snapshot(view: &Entity<LabView>, cx: &mut AsyncApp) -> Option<Snapshot> {
    view.read_with(cx, |view, cx| {
        let player = view.player()?.read(cx);
        Some(Snapshot {
            state: player.state().clone(),
            stats: player.stats(),
            info: player.info().clone(),
            last_seek: view.last_seek(),
        })
    })
}

/// Polls the player until `done` holds or `timeout` passes. Returns the
/// last snapshot and whether `done` held.
async fn wait_until(
    view: &Entity<LabView>,
    cx: &mut AsyncApp,
    timeout: Duration,
    done: impl Fn(&Snapshot) -> bool,
) -> Option<(Snapshot, bool)> {
    let started = Instant::now();
    loop {
        let now = snapshot(view, cx)?;
        if done(&now) {
            return Some((now, true));
        }
        if started.elapsed() >= timeout {
            return Some((now, false));
        }
        cx.background_executor().timer(POLL).await;
    }
}

fn millis(duration: Duration) -> f64 {
    (duration.as_secs_f64() * 1e5).round() / 100.0
}

fn megabytes(bytes: u64) -> f64 {
    (bytes as f64 / 1_048_576.0 * 10.0).round() / 10.0
}

fn rounded(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// CPU used between two readings, as a share of one core.
fn cpu_percent(before: Option<Duration>, after: Option<Duration>, wall: Duration) -> Option<f64> {
    let used = after?.saturating_sub(before?);
    (wall > Duration::ZERO).then(|| rounded(used.as_secs_f64() * 100.0 / wall.as_secs_f64()))
}

/// Plays every job on every backend in `backends` (indexes into the lab's
/// backend list) and reports. `play_seconds` is how long each clip is
/// measured for, unless its manifest entry says otherwise.
pub async fn run(
    view: &Entity<LabView>,
    jobs: &[Job],
    backends: &[usize],
    play_seconds: f64,
    cx: &mut AsyncApp,
) -> Report {
    let started_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let mut results = Vec::new();
    for &backend in backends {
        for job in jobs {
            if let Some(result) = run_one(view, job, backend, play_seconds, cx).await {
                results.push(result);
            }
        }
    }
    view.update(cx, |view, cx| view.close(cx));
    let passed = results.iter().filter(|result| result.pass).count();
    Report {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        started_unix,
        passed,
        failed: results.len() - passed,
        results,
    }
}

/// Benches one clip on one backend. `None` only when the window is gone.
async fn run_one(
    view: &Entity<LabView>,
    job: &Job,
    backend: usize,
    play_seconds: f64,
    cx: &mut AsyncApp,
) -> Option<ClipResult> {
    let expected = job.expected.as_ref();
    let must_fail = expected.is_some_and(|expected| expected.expect == Outcome::Fails);
    let cpu_before = metrics::cpu_time();
    let opened = Instant::now();
    view.update(cx, |view, cx| view.select(job.clip, backend, cx));

    // 1. The first frame, or a failure.
    let (mut last, started) =
        wait_until(view, cx, START_TIMEOUT, |now| now.failed() || now.started()).await?;
    let time_to_first_frame =
        (started && last.stats.frames_presented > 0).then(|| opened.elapsed());
    let mut timed_out = !started;

    // 2. Play and count, or wait for the failure the manifest promises.
    let at_start = last.stats;
    let measured_from = Instant::now();
    let mut played = Duration::ZERO;
    if started && !last.failed() {
        if must_fail {
            let (now, failed) = wait_until(view, cx, FAIL_TIMEOUT, Snapshot::failed).await?;
            last = now;
            timed_out = !failed;
        } else {
            let wanted = expected
                .and_then(|expected| expected.play_seconds)
                .unwrap_or(play_seconds);
            let room = last
                .info
                .duration
                .map_or(wanted, |duration| duration.as_secs_f64() - END_MARGIN);
            let window = Duration::from_secs_f64(wanted.min(room).max(0.1));
            let (now, _) = wait_until(view, cx, window, |now| {
                now.failed() || now.state == PlaybackState::Ended
            })
            .await?;
            last = now;
        }
        played = measured_from.elapsed();
    }
    let frames_presented = last.stats.frames_presented - at_start.frames_presented;
    let frames_dropped = last.stats.frames_dropped - at_start.frames_dropped;

    // 3. A seek to the middle, timed.
    let mut seek = None;
    let can_seek = last.info.has_video
        && last
            .info
            .duration
            .is_some_and(|duration| duration >= Duration::from_secs(2));
    if started && !must_fail && !last.failed() && can_seek {
        let duration = last.info.duration.unwrap_or_default();
        let position = view.read_with(cx, |view, cx| {
            view.player()
                .map_or(Duration::ZERO, |player| player.read(cx).position())
        });
        let target = seek_target(duration, position);
        view.update(cx, |view, cx| view.seek_to(target, cx));
        let (now, finished) = wait_until(view, cx, SEEK_TIMEOUT, |now| {
            now.failed() || now.last_seek.is_some()
        })
        .await?;
        seek = now.last_seek;
        if !finished {
            timed_out = true;
        }
        last = now;
    }

    let wall = opened.elapsed();
    let cpu = cpu_percent(cpu_before, metrics::cpu_time(), wall);
    let memory = metrics::resident_memory();
    let (backend_name, presenter) = view.read_with(cx, |view, cx| {
        view.player().map(|player| {
            let player = player.read(cx);
            (player.backend_name(), player.presenter())
        })
    })?;
    view.update(cx, |view, cx| view.close(cx));

    let error = match &last.state {
        PlaybackState::Failed(error) => Some(ErrorReport {
            kind: format!("{:?}", error.kind()),
            message: error.message().to_string(),
        }),
        _ => None,
    };
    let outcome = match (&error, timed_out) {
        (Some(_), _) => "failed",
        (None, true) => "timed_out",
        (None, false) => "played",
    };
    let checks = expected
        .filter(|_| outcome == "played")
        .map(|expected| expected.check(&last.info));
    let problems = problems(expected, outcome, error.as_ref(), checks.as_ref());
    let due = frames_presented + frames_dropped;
    Some(ClipResult {
        clip: job.name.clone(),
        backend: backend_name,
        presenter,
        outcome,
        pass: problems.is_empty(),
        problems,
        error,
        time_to_first_frame_ms: time_to_first_frame.map(millis),
        seek_ms: seek.map(millis),
        played_seconds: rounded(played.as_secs_f64()),
        frames_presented,
        frames_dropped,
        dropped_percent: (due > 0).then(|| rounded(frames_dropped as f64 * 100.0 / due as f64)),
        video_fps: (frames_presented > 0 && played > Duration::ZERO)
            .then(|| rounded(frames_presented as f64 / played.as_secs_f64())),
        max_drift_ms: last.stats.max_drift.map(millis),
        cpu_percent: cpu,
        memory_mb: memory.map(megabytes),
        reported: Reported::from(&last.info),
        expected: job.expected.clone(),
        checks,
    })
}

/// Where the timed seek goes: the middle, unless playback is already within
/// a second of it (a clip measured for half its length is), in which case a
/// quarter of the way in, so the seek has somewhere to go.
fn seek_target(duration: Duration, position: Duration) -> Duration {
    let middle = duration / 2;
    if position.abs_diff(middle) < Duration::from_secs(1) {
        duration / 4
    } else {
        middle
    }
}

/// Everything that makes a result not pass, in words.
fn problems(
    expected: Option<&Expected>,
    outcome: &str,
    error: Option<&ErrorReport>,
    checks: Option<&Checks>,
) -> Vec<String> {
    let mut problems = Vec::new();
    let must_fail = expected.is_some_and(|expected| expected.expect == Outcome::Fails);
    match (must_fail, outcome, error) {
        (false, "played", _) => {}
        (false, _, Some(error)) => {
            problems.push(format!("failed with {}: {}", error.kind, error.message));
        }
        (true, "failed", Some(error)) => {
            let allowed = &expected.expect("must_fail implies an entry").errors;
            if !allowed.contains(&error.kind) {
                problems.push(format!(
                    "failed with {}, expected {}",
                    error.kind,
                    allowed.join(" or ")
                ));
            }
        }
        (true, "played", _) => problems.push("played, but it must fail".to_string()),
        _ => problems.push("timed out".to_string()),
    }
    if let Some(checks) = checks {
        for (name, check) in [
            ("size", checks.size),
            ("rotation", checks.rotation),
            ("duration", checks.duration),
            ("tracks", checks.tracks),
            ("colour range", checks.color_range),
            ("colour matrix", checks.color_matrix),
        ] {
            if check == Some(false) {
                problems.push(format!("{name} does not match the manifest"));
            }
        }
    }
    problems
}

/// A soak's output: one clip looping, sampled as it goes.
#[derive(Debug, Serialize)]
pub struct SoakReport {
    /// The operating system.
    pub os: &'static str,
    /// The CPU architecture.
    pub arch: &'static str,
    /// The clip's name.
    pub clip: String,
    /// The backend's name.
    pub backend: &'static str,
    /// How long the soak ran.
    pub seconds: f64,
    /// Resident memory at the last sample minus the first, or `None` where
    /// memory cannot be read.
    pub memory_growth_mb: Option<f64>,
    /// CPU over the whole soak, as a share of one core.
    pub cpu_percent: Option<f64>,
    /// Frames shown.
    pub frames_presented: u64,
    /// Frames dropped.
    pub frames_dropped: u64,
    /// The largest drift seen.
    pub max_drift_ms: Option<f64>,
    /// The state at the end: `Playing` unless something went wrong.
    pub final_state: String,
    /// One entry every few seconds.
    pub samples: Vec<SoakSample>,
}

/// The process and player at one moment of a soak.
#[derive(Debug, Serialize)]
pub struct SoakSample {
    /// Seconds since the soak began.
    pub at_seconds: f64,
    /// Resident memory.
    pub memory_mb: Option<f64>,
    /// CPU since the previous sample, as a share of one core.
    pub cpu_percent: Option<f64>,
    /// Frames shown so far.
    pub frames_presented: u64,
    /// Frames dropped so far.
    pub frames_dropped: u64,
}

/// Loops one clip for `seconds` and samples memory, CPU and frame counts.
/// `None` only when the window is gone.
pub async fn soak(
    view: &Entity<LabView>,
    job: &Job,
    backend: usize,
    seconds: f64,
    cx: &mut AsyncApp,
) -> Option<SoakReport> {
    let length = Duration::from_secs_f64(seconds.max(0.0));
    view.update(cx, |view, cx| {
        view.change_settings(|settings| settings.looping = true, cx);
        view.select(job.clip, backend, cx);
    });
    let started = Instant::now();
    let cpu_at_start = metrics::cpu_time();
    let mut samples: Vec<SoakSample> = Vec::new();
    let (mut sampled_at, mut cpu_at_sample) = (started, cpu_at_start);
    let last = loop {
        let left = length.saturating_sub(started.elapsed());
        let (now, failed) = wait_until(view, cx, SOAK_SAMPLE.min(left), Snapshot::failed).await?;
        let cpu = metrics::cpu_time();
        samples.push(SoakSample {
            at_seconds: rounded(started.elapsed().as_secs_f64()),
            memory_mb: metrics::resident_memory().map(megabytes),
            cpu_percent: cpu_percent(cpu_at_sample, cpu, sampled_at.elapsed()),
            frames_presented: now.stats.frames_presented,
            frames_dropped: now.stats.frames_dropped,
        });
        (sampled_at, cpu_at_sample) = (Instant::now(), cpu);
        if failed || started.elapsed() >= length {
            break now;
        }
    };
    let backend_name = view.read_with(cx, |view, cx| {
        view.player().map(|player| player.read(cx).backend_name())
    })?;
    view.update(cx, |view, cx| view.close(cx));
    let growth = samples
        .first()
        .zip(samples.last())
        .and_then(|(first, last)| Some(rounded(last.memory_mb? - first.memory_mb?)));
    Some(SoakReport {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        clip: job.name.clone(),
        backend: backend_name,
        seconds: rounded(started.elapsed().as_secs_f64()),
        memory_growth_mb: growth,
        cpu_percent: cpu_percent(cpu_at_start, metrics::cpu_time(), started.elapsed()),
        frames_presented: last.stats.frames_presented,
        frames_dropped: last.stats.frames_dropped,
        max_drift_ms: last.stats.max_drift.map(millis),
        final_state: format!("{:?}", last.state),
        samples,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{TestAppContext, VisualTestContext};
    use gpui_av::{Backend, Source};

    use super::*;
    use crate::clips::Clip;
    use crate::test_pattern::TestPatternBackend;
    use crate::view::Settings;

    /// Runs GPUI's test clock and redraws the window until `done`, as a
    /// real window's frames would. The pattern and the bench both read
    /// the real clock, so this takes about as long as the clips it plays.
    fn pump(cx: &mut VisualTestContext, done: impl Fn() -> bool) {
        let started = Instant::now();
        while !done() {
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "the bench hung"
            );
            cx.executor().advance_clock(POLL);
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[gpui::test]
    fn the_bench_plays_checks_and_seeks_through_the_public_api(cx: &mut TestAppContext) {
        let entries = [
            Expected {
                file: "pattern://320x180?fps=30&duration=3".into(),
                width: Some(320),
                height: Some(180),
                rotation: Some(0),
                duration: Some(3.0),
                video_codec: Some("pattern".into()),
                // The pattern reports no colour; that is unknown, not wrong.
                color_range: Some("limited".into()),
                ..Expected::default()
            },
            Expected {
                file: "pattern://bad".into(),
                expect: Outcome::Fails,
                errors: vec!["Unsupported".into()],
                ..Expected::default()
            },
            // The manifest is wrong about this one, and the bench says so.
            Expected {
                file: "pattern://320x180?fps=30&duration=4".into(),
                width: Some(640),
                height: Some(360),
                video_codec: Some("pattern".into()),
                ..Expected::default()
            },
        ];
        let clips = entries
            .iter()
            .map(|entry| Clip {
                label: entry.file.clone(),
                source: Source::url(&entry.file),
            })
            .collect();
        let jobs: Vec<_> = entries
            .iter()
            .enumerate()
            .map(|(clip, entry)| Job {
                clip,
                name: entry.file.clone(),
                expected: Some(entry.clone()),
            })
            .collect();
        let backends: Vec<Rc<dyn Backend>> = vec![Rc::new(TestPatternBackend)];
        let (view, cx) = cx
            .add_window_view(|_, cx| LabView::new(backends, clips, Settings::default(), false, cx));

        let report = Rc::new(RefCell::new(None));
        cx.update(|_, cx| {
            let report = report.clone();
            cx.spawn(async move |cx| {
                let finished = run(&view, &jobs, &[0], 0.4, cx).await;
                *report.borrow_mut() = Some(finished);
            })
            .detach();
        });
        pump(cx, || report.borrow().is_some());
        let report = report.take().unwrap();

        assert_eq!((report.passed, report.failed), (2, 1));
        let [played, refused, mismatched] = report.results.as_slice() else {
            panic!("expected three results, got {}", report.results.len());
        };

        assert_eq!((played.outcome, played.backend), ("played", "test-pattern"));
        assert_eq!(played.presenter, "image");
        assert!(played.pass, "{:?}", played.problems);
        assert!(played.time_to_first_frame_ms.is_some());
        assert!(played.seek_ms.is_some());
        assert!(played.frames_presented >= 2, "{}", played.frames_presented);
        assert!(played.max_drift_ms.is_some());
        assert_eq!(played.reported.duration, Some(3.0));
        let checks = played.checks.unwrap();
        assert_eq!((checks.size, checks.color_range), (Some(true), None));

        assert_eq!(refused.outcome, "failed");
        assert!(refused.pass, "{:?}", refused.problems);
        assert_eq!(refused.error.as_ref().unwrap().kind, "Unsupported");

        assert_eq!(mismatched.outcome, "played");
        assert_eq!(mismatched.problems, ["size does not match the manifest"]);

        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["results"][0]["reported"]["width"], 320);
        assert_eq!(json["results"][1]["error"]["kind"], "Unsupported");
    }

    #[gpui::test]
    fn a_soak_loops_and_samples(cx: &mut TestAppContext) {
        let clip = Clip {
            label: "short".into(),
            source: Source::url("pattern://160x90?fps=30&duration=0.2"),
        };
        let job = Job {
            clip: 0,
            name: clip.label.clone(),
            expected: None,
        };
        let backends: Vec<Rc<dyn Backend>> = vec![Rc::new(TestPatternBackend)];
        let (view, cx) = cx.add_window_view(|_, cx| {
            LabView::new(backends, vec![clip], Settings::default(), false, cx)
        });

        let report = Rc::new(RefCell::new(None));
        cx.update(|_, cx| {
            let report = report.clone();
            cx.spawn(async move |cx| {
                *report.borrow_mut() = Some(soak(&view, &job, 0, 0.7, cx).await);
            })
            .detach();
        });
        pump(cx, || report.borrow().is_some());
        let report = report.take().unwrap().expect("the window stayed open");

        // Three loops of a 0.2 s clip, and it is still playing.
        assert_eq!(report.final_state, "Playing");
        assert_eq!(report.samples.len(), 1);
        assert!(report.frames_presented >= 3, "{}", report.frames_presented);
        assert!(report.seconds >= 0.7);
    }

    fn fails_with(errors: &[&str]) -> Expected {
        Expected {
            file: "clip".into(),
            expect: Outcome::Fails,
            errors: errors.iter().map(|kind| kind.to_string()).collect(),
            ..Expected::default()
        }
    }

    fn error(kind: &str) -> ErrorReport {
        ErrorReport {
            kind: kind.into(),
            message: "why".into(),
        }
    }

    #[test]
    fn a_clip_that_plays_and_matches_has_no_problems() {
        let expected = Expected::default();
        assert!(problems(Some(&expected), "played", None, Some(&Checks::default())).is_empty());
        assert!(problems(None, "played", None, None).is_empty());
    }

    #[test]
    fn an_unexpected_failure_or_timeout_is_a_problem() {
        let expected = Expected::default();
        assert_eq!(
            problems(Some(&expected), "failed", Some(&error("Decode")), None),
            ["failed with Decode: why"]
        );
        assert_eq!(problems(None, "timed_out", None, None), ["timed out"]);
    }

    #[test]
    fn a_clip_that_must_fail_passes_only_with_a_listed_kind() {
        let expected = fails_with(&["Decode", "Unsupported"]);
        assert!(problems(Some(&expected), "failed", Some(&error("Unsupported")), None).is_empty());
        assert_eq!(
            problems(Some(&expected), "failed", Some(&error("Network")), None),
            ["failed with Network, expected Decode or Unsupported"]
        );
        assert_eq!(
            problems(Some(&expected), "played", None, None),
            ["played, but it must fail"]
        );
        assert_eq!(
            problems(Some(&expected), "timed_out", None, None),
            ["timed out"]
        );
    }

    #[test]
    fn each_mismatch_is_named() {
        let checks = Checks {
            size: Some(false),
            color_matrix: Some(false),
            rotation: Some(true),
            ..Checks::default()
        };
        assert_eq!(
            problems(Some(&Expected::default()), "played", None, Some(&checks)),
            [
                "size does not match the manifest",
                "colour matrix does not match the manifest"
            ]
        );
    }

    #[test]
    fn the_seek_never_targets_where_playback_already_is() {
        let ten = Duration::from_secs(10);
        assert_eq!(
            seek_target(ten, Duration::from_secs(2)),
            Duration::from_secs(5)
        );
        assert_eq!(
            seek_target(ten, Duration::from_millis(5020)),
            Duration::from_millis(2500)
        );
    }

    #[test]
    fn cpu_is_a_share_of_one_core() {
        let (before, after) = (Duration::from_secs(1), Duration::from_millis(1500));
        assert_eq!(
            cpu_percent(Some(before), Some(after), Duration::from_secs(2)),
            Some(25.0)
        );
        assert_eq!(cpu_percent(None, Some(after), Duration::from_secs(2)), None);
    }
}
