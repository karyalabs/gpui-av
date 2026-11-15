//! The Media Foundation backend end to end, without a window: a headless
//! GPUI app plays a clip through the public API, muted, and checks what it
//! reports. Windows only.
//!
//! The clip comes from `GPUI_AV_TEST_CLIP` (an MP4 with H.264 and AAC, at
//! least 4 s long; the Windows workflow makes one with ffmpeg). Without it
//! the test says so and passes, so `cargo test` works on any machine.

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() {
    windows_test::main();
}

#[cfg(windows)]
mod windows_test {
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use gpui::{App, AppContext, AsyncApp, Entity};
    use gpui_av::{Backend, ErrorKind, PlaybackState, PlayerEvent, Source, VideoPlayer};
    use gpui_av_mediafoundation::MediaFoundationBackend;

    struct Report {
        failures: Vec<String>,
    }

    impl Report {
        fn check(&mut self, name: &str, ok: bool, detail: impl std::fmt::Debug) {
            println!("{} {name}: {detail:?}", if ok { "ok  " } else { "FAIL" });
            if !ok {
                self.failures.push(name.to_string());
            }
        }
    }

    fn backend() -> Rc<dyn Backend> {
        Rc::new(MediaFoundationBackend::new())
    }

    async fn wait(cx: &mut AsyncApp, millis: u64) {
        cx.background_executor()
            .timer(Duration::from_millis(millis))
            .await;
    }

    /// Waits up to `limit` for the player to reach a state `done` accepts.
    async fn wait_for(
        player: &Entity<VideoPlayer>,
        limit: Duration,
        done: impl Fn(&PlaybackState) -> bool,
        cx: &mut AsyncApp,
    ) -> PlaybackState {
        let started = Instant::now();
        loop {
            let state = player.read_with(cx, |player, _| player.state().clone());
            if done(&state) || started.elapsed() > limit {
                return state;
            }
            wait(cx, 10).await;
        }
    }

    fn failure_kind(state: &PlaybackState) -> Option<ErrorKind> {
        match state {
            PlaybackState::Failed(error) => Some(error.kind()),
            _ => None,
        }
    }

    async fn checks(clip: PathBuf, report: &mut Report, cx: &mut AsyncApp) {
        // Playing a file.
        let opened = Instant::now();
        let player = cx.new(|cx| {
            let mut player = VideoPlayer::with_backend(Source::file(&clip), backend(), cx);
            player.set_muted(true, cx);
            player.play();
            player
        });
        let seeks = Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = seeks.clone();
        let _events = cx.update(|cx| {
            cx.subscribe(&player, move |_, event: &PlayerEvent, _| {
                if let PlayerEvent::Seeked(position) = event {
                    seen.borrow_mut().push(*position);
                }
            })
        });
        let state = wait_for(
            &player,
            Duration::from_secs(15),
            |state| matches!(state, PlaybackState::Playing | PlaybackState::Failed(_)),
            cx,
        )
        .await;
        report.check(
            "a file plays",
            state == PlaybackState::Playing,
            (&state, opened.elapsed()),
        );
        let info = player.read_with(cx, |player, _| player.info().clone());
        report.check(
            "media info: video, audio, size and length",
            info.has_video
                && info.has_audio == Some(true)
                && info.natural_size.is_some()
                && info
                    .duration
                    .is_some_and(|length| length >= Duration::from_secs(4)),
            &info,
        );
        let before = player.read_with(cx, |player, _| player.position());
        wait(cx, 700).await;
        let after = player.read_with(cx, |player, _| player.position());
        report.check(
            "the position moves on",
            after > before + Duration::from_millis(300),
            (before, after),
        );
        let buffered = player.read_with(cx, |player, _| player.buffered());
        report.check(
            "a playing file reports what is buffered",
            !buffered.is_empty(),
            &buffered,
        );

        player.update(cx, |player, _| player.seek(Duration::from_secs(2)));
        let started = Instant::now();
        while seeks.borrow().is_empty() && started.elapsed() < Duration::from_secs(5) {
            wait(cx, 10).await;
        }
        let landed = seeks.borrow().first().copied();
        report.check(
            "a seek reports where it landed",
            landed.is_some_and(|position| {
                position >= Duration::from_millis(1900) && position <= Duration::from_millis(2600)
            }),
            (landed, started.elapsed()),
        );

        player.update(cx, |player, _| player.pause());
        let state = wait_for(
            &player,
            Duration::from_secs(5),
            |state| *state == PlaybackState::Paused,
            cx,
        )
        .await;
        report.check("pause", state == PlaybackState::Paused, &state);
        cx.update(|_| drop(player));

        // Failures.
        let missing = clip.with_file_name("does-not-exist.mp4");
        let player = cx.new(|cx| VideoPlayer::with_backend(Source::file(missing), backend(), cx));
        let state = wait_for(
            &player,
            Duration::from_secs(10),
            |state| matches!(state, PlaybackState::Failed(_)),
            cx,
        )
        .await;
        report.check(
            "a missing file: NotFound",
            failure_kind(&state) == Some(ErrorKind::NotFound),
            &state,
        );

        let junk = std::env::temp_dir().join("gpui-av-junk.mp4");
        let bytes: Vec<u8> = (0..65536u32)
            .map(|value| (value.wrapping_mul(2_654_435_761) >> 24) as u8)
            .collect();
        if let Err(error) = std::fs::write(&junk, bytes) {
            report.check("write a junk file", false, error);
        }
        let player = cx.new(|cx| VideoPlayer::with_backend(Source::file(&junk), backend(), cx));
        let state = wait_for(
            &player,
            Duration::from_secs(10),
            |state| matches!(state, PlaybackState::Failed(_)),
            cx,
        )
        .await;
        report.check(
            "random bytes: Unsupported or Decode",
            matches!(
                failure_kind(&state),
                Some(ErrorKind::Unsupported | ErrorKind::Decode)
            ),
            &state,
        );

        let with_header = Source::url("https://example.test/clip.mp4").header("Authorization", "x");
        let player = cx.new(|cx| VideoPlayer::with_backend(with_header, backend(), cx));
        let state = player.read_with(cx, |player, _| player.state().clone());
        report.check(
            "request headers are refused, not dropped",
            failure_kind(&state) == Some(ErrorKind::Unsupported),
            &state,
        );
    }

    pub fn main() {
        let Some(clip) = std::env::var_os("GPUI_AV_TEST_CLIP") else {
            println!("skipped: set GPUI_AV_TEST_CLIP to an MP4 with H.264 and AAC");
            return;
        };
        let clip = PathBuf::from(clip);
        gpui_platform::headless().run(move |cx: &mut App| {
            cx.spawn(async move |cx| {
                let mut report = Report {
                    failures: Vec::new(),
                };
                checks(clip, &mut report, cx).await;
                // GPUI's quit ends the process with success whatever
                // happened, so report and exit with the right code here.
                if report.failures.is_empty() {
                    println!("all passed");
                    std::process::exit(0);
                }
                println!("failed: {:#?}", report.failures);
                std::process::exit(1);
            })
            .detach();
        });
        println!("the app quit before the checks finished");
        std::process::exit(1);
    }
}
