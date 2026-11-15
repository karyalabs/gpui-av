//! Drives a `VideoPlayer` through a scripted backend that uses only the
//! public `Backend` trait, as any third-party backend would.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{App, AppContext, Entity, TestAppContext, Window, size};
use gpui_av::{
    Backend, Error, ErrorKind, EventSink, MediaInfo, Placement, PlaybackState, PlayerEvent,
    Session, Source, Stats, VideoPlayer,
};

/// What the scripted backend saw, shared with the test.
#[derive(Default)]
struct Log {
    opened: Vec<Source>,
    commands: Vec<String>,
    sink: Option<EventSink>,
    sessions_dropped: usize,
    /// How many times the element mounted, and how many of those ended.
    mounted: (usize, usize),
    /// The session shows its frames outside the window, as an overlay does.
    presents_itself: bool,
}

/// What the scripted session hands out from `mount`.
struct MountGuard(Rc<RefCell<Log>>);

impl Drop for MountGuard {
    fn drop(&mut self) {
        self.0.borrow_mut().mounted.1 += 1;
    }
}

struct Scripted {
    log: Rc<RefCell<Log>>,
    refuse: Option<Error>,
}

impl Backend for Scripted {
    fn name(&self) -> &'static str {
        "scripted"
    }

    fn open(
        &self,
        source: Source,
        events: EventSink,
        _cx: &mut App,
    ) -> Result<Box<dyn Session>, Error> {
        if let Some(error) = &self.refuse {
            return Err(error.clone());
        }
        let mut log = self.log.borrow_mut();
        log.opened.push(source);
        log.sink = Some(events);
        Ok(Box::new(ScriptedSession {
            log: self.log.clone(),
        }))
    }
}

struct ScriptedSession {
    log: Rc<RefCell<Log>>,
}

impl ScriptedSession {
    fn record(&self, command: impl Into<String>) {
        self.log.borrow_mut().commands.push(command.into());
    }
}

impl Session for ScriptedSession {
    fn play(&mut self) {
        self.record("play");
    }
    fn pause(&mut self) {
        self.record("pause");
    }
    fn seek(&mut self, position: Duration) {
        self.record(format!("seek {}", position.as_millis()));
    }
    fn set_volume(&mut self, volume: f32) {
        self.record(format!("volume {volume}"));
    }
    fn set_muted(&mut self, muted: bool) {
        self.record(format!("muted {muted}"));
    }
    fn set_looping(&mut self, looping: bool) {
        self.record(format!("looping {looping}"));
    }
    fn position(&self) -> Duration {
        Duration::from_secs(7)
    }
    fn buffered(&self) -> Vec<std::ops::Range<Duration>> {
        vec![Duration::ZERO..Duration::from_secs(12)]
    }
    fn stats(&self) -> Stats {
        Stats::new(120, 2)
    }
    fn mount(&mut self) -> Option<Box<dyn std::any::Any>> {
        self.log.borrow_mut().mounted.0 += 1;
        Some(Box::new(MountGuard(self.log.clone())))
    }
    fn needs_frames(&self) -> bool {
        !self.log.borrow().presents_itself
    }
    fn paint(&mut self, _placement: Placement, _window: &mut Window, _cx: &mut App) {}
}

impl Drop for ScriptedSession {
    fn drop(&mut self) {
        self.log.borrow_mut().sessions_dropped += 1;
    }
}

fn scripted() -> (Rc<dyn Backend>, Rc<RefCell<Log>>) {
    let log = Rc::new(RefCell::new(Log::default()));
    let backend = Rc::new(Scripted {
        log: log.clone(),
        refuse: None,
    });
    (backend, log)
}

fn source() -> Source {
    Source::url("https://example.test/clip.mp4").header("Authorization", "token")
}

/// Collects every event the player emits.
fn record_events(
    player: &Entity<VideoPlayer>,
    cx: &mut TestAppContext,
) -> Rc<RefCell<Vec<PlayerEvent>>> {
    let events = Rc::new(RefCell::new(Vec::new()));
    let seen = events.clone();
    cx.update(|cx| {
        cx.subscribe(player, move |_, event: &PlayerEvent, _| {
            seen.borrow_mut().push(event.clone());
        })
        .detach();
    });
    events
}

fn sink(log: &Rc<RefCell<Log>>) -> EventSink {
    log.borrow().sink.clone().expect("the backend was opened")
}

#[gpui::test]
fn the_default_backend_opens_the_source(cx: &mut TestAppContext) {
    let (backend, log) = scripted();
    cx.update(|cx| gpui_av::set_default_backend(backend, cx));
    let player = cx.new(|cx| VideoPlayer::new(source(), cx));

    assert_eq!(log.borrow().opened, [source()]);
    player.read_with(cx, |player, _| {
        assert_eq!(*player.state(), PlaybackState::Loading);
        assert_eq!(player.backend_name(), "scripted");
    });
}

#[gpui::test]
fn without_a_backend_the_player_fails(cx: &mut TestAppContext) {
    let player = cx.new(|cx| VideoPlayer::new(source(), cx));
    player.read_with(cx, |player, _| match player.state() {
        PlaybackState::Failed(error) => assert_eq!(error.kind(), ErrorKind::NoBackend),
        other => panic!("expected a failure, got {other:?}"),
    });
}

#[gpui::test]
fn a_failure_at_open_reaches_a_subscriber(cx: &mut TestAppContext) {
    let refusal = Error::new(ErrorKind::Unsupported, "cannot send request headers");
    let backend: Rc<dyn Backend> = Rc::new(Scripted {
        log: Rc::default(),
        refuse: Some(refusal.clone()),
    });
    // Created and subscribed to in one go, as a view does.
    let events = Rc::new(RefCell::new(Vec::new()));
    let seen = events.clone();
    let player = cx.update(|cx| {
        let player = cx.new(|cx| VideoPlayer::with_backend(source(), backend, cx));
        cx.subscribe(&player, move |_, event: &PlayerEvent, _| {
            seen.borrow_mut().push(event.clone());
        })
        .detach();
        player
    });
    assert!(events.borrow().is_empty());

    cx.run_until_parked();
    assert_eq!(
        *events.borrow(),
        [PlayerEvent::StateChanged(PlaybackState::Failed(refusal))]
    );
    // Once only.
    cx.run_until_parked();
    assert_eq!(events.borrow().len(), 1);
    drop(player);
}

#[gpui::test]
fn a_refused_source_fails_at_once(cx: &mut TestAppContext) {
    let refusal = Error::new(ErrorKind::Unsupported, "cannot send request headers");
    let backend: Rc<dyn Backend> = Rc::new(Scripted {
        log: Rc::default(),
        refuse: Some(refusal.clone()),
    });
    let player = cx.new(|cx| VideoPlayer::with_backend(source(), backend, cx));
    player.read_with(cx, |player, _| {
        assert_eq!(*player.state(), PlaybackState::Failed(refusal));
    });
}

#[gpui::test]
fn backend_events_become_state_and_player_events(cx: &mut TestAppContext) {
    let (backend, log) = scripted();
    let player = cx.new(|cx| VideoPlayer::with_backend(source(), backend, cx));
    let events = record_events(&player, cx);
    let sink = sink(&log);

    let info = MediaInfo {
        duration: Some(Duration::from_secs(20)),
        natural_size: Some(size(1920, 1080)),
        has_video: true,
        has_audio: Some(true),
        ..MediaInfo::default()
    };
    sink.info(info.clone());
    sink.state(PlaybackState::Ready);
    sink.state(PlaybackState::Ready);
    sink.state(PlaybackState::Playing);
    sink.seeked(Duration::from_secs(3));
    cx.run_until_parked();

    player.read_with(cx, |player, _| {
        assert_eq!(*player.info(), info);
        assert_eq!(player.duration(), Some(Duration::from_secs(20)));
        assert_eq!(player.natural_size(), Some(size(1920, 1080)));
        assert_eq!(*player.state(), PlaybackState::Playing);
        assert_eq!(player.position(), Duration::from_secs(7));
        assert_eq!(player.stats(), Stats::new(120, 2));
        assert_eq!(player.buffered(), [Duration::ZERO..Duration::from_secs(12)]);
    });
    // The repeated Ready is not announced twice.
    assert_eq!(
        *events.borrow(),
        [
            PlayerEvent::InfoChanged,
            PlayerEvent::StateChanged(PlaybackState::Ready),
            PlayerEvent::StateChanged(PlaybackState::Playing),
            PlayerEvent::Seeked(Duration::from_secs(3)),
        ]
    );
}

#[gpui::test]
fn events_from_another_thread_arrive(cx: &mut TestAppContext) {
    let (backend, log) = scripted();
    let player = cx.new(|cx| VideoPlayer::with_backend(source(), backend, cx));
    let sink = sink(&log);

    std::thread::spawn(move || sink.state(PlaybackState::Ready))
        .join()
        .unwrap();
    cx.run_until_parked();

    player.read_with(cx, |player, _| {
        assert_eq!(*player.state(), PlaybackState::Ready);
    });
}

#[gpui::test]
fn commands_reach_the_session(cx: &mut TestAppContext) {
    let (backend, log) = scripted();
    let player = cx.new(|cx| VideoPlayer::with_backend(source(), backend, cx));

    // Commands are accepted while the media is still loading.
    player.update(cx, |player, cx| {
        player.play();
        player.pause();
        player.seek(Duration::from_millis(1500));
        player.set_volume(4.0, cx);
        player.set_muted(true, cx);
        player.set_looping(true, cx);

        assert_eq!(player.volume(), 1.0);
        assert!(player.muted());
        assert!(player.looping());
    });
    assert_eq!(
        log.borrow().commands,
        [
            "play",
            "pause",
            "seek 1500",
            "volume 1",
            "muted true",
            "looping true"
        ]
    );
}

#[gpui::test]
fn a_failure_is_final_and_frees_the_session(cx: &mut TestAppContext) {
    let (backend, log) = scripted();
    let player = cx.new(|cx| VideoPlayer::with_backend(source(), backend, cx));
    let sink = sink(&log);
    let error = Error::new(ErrorKind::Unauthorized, "HTTP 401");

    sink.fail(error.clone());
    sink.state(PlaybackState::Playing);
    cx.run_until_parked();

    assert_eq!(log.borrow().sessions_dropped, 1);
    player.update(cx, |player, _| {
        assert_eq!(*player.state(), PlaybackState::Failed(error));
        assert_eq!(player.position(), Duration::ZERO);
        assert!(player.buffered().is_empty());
        // The counts from before the failure are kept.
        assert_eq!(player.stats(), Stats::new(120, 2));
        // Commands on a failed player do nothing.
        player.play();
    });
    assert!(log.borrow().commands.is_empty());
}

#[gpui::test]
fn dropping_the_player_drops_the_session(cx: &mut TestAppContext) {
    let (backend, log) = scripted();
    let player = cx.new(|cx| VideoPlayer::with_backend(source(), backend, cx));

    cx.update(|_| drop(player));
    cx.run_until_parked();

    assert_eq!(log.borrow().sessions_dropped, 1);
}

/// A view that shows the player's `video()` element, or nothing.
struct Stage {
    player: Entity<VideoPlayer>,
    shown: bool,
}

impl gpui::Render for Stage {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        use gpui::{ParentElement, Styled};
        let stage = gpui::div().size_full();
        if self.shown {
            stage.child(gpui_av::video(&self.player).size_full())
        } else {
            stage
        }
    }
}

#[gpui::test]
fn the_session_is_mounted_while_its_element_shows_the_picture(cx: &mut TestAppContext) {
    let (backend, log) = scripted();
    let player = cx.new(|cx| VideoPlayer::with_backend(source(), backend, cx));
    let (stage, cx) = cx.add_window_view({
        let player = player.clone();
        |_, _| Stage {
            player,
            shown: true,
        }
    });
    cx.run_until_parked();
    assert_eq!(
        log.borrow().mounted,
        (0, 0),
        "nothing to show before the size of the picture is known"
    );

    sink(&log).info(MediaInfo {
        natural_size: Some(size(1920, 1080)),
        has_video: true,
        ..MediaInfo::default()
    });
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert_eq!(
        log.borrow().mounted,
        (1, 0),
        "mounted once, still on screen"
    );

    // Drawing again does not mount again.
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert_eq!(log.borrow().mounted, (1, 0));

    // The element leaves; the player lives on.
    stage.update(cx, |stage, cx| {
        stage.shown = false;
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(
        log.borrow().mounted,
        (1, 1),
        "unmounted by the frame without it"
    );
    assert_eq!(log.borrow().sessions_dropped, 0);

    // And comes back.
    stage.update(cx, |stage, cx| {
        stage.shown = true;
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(log.borrow().mounted, (2, 1));
}

/// Draws the stage with a playing picture and says how many callbacks the
/// window then holds for its next frame.
fn frames_asked_for(presents_itself: bool, cx: &mut TestAppContext) -> usize {
    let (backend, log) = scripted();
    log.borrow_mut().presents_itself = presents_itself;
    let player = cx.new(|cx| VideoPlayer::with_backend(source(), backend, cx));
    let (_stage, cx) = cx.add_window_view({
        let player = player.clone();
        |_, _| Stage {
            player,
            shown: true,
        }
    });
    sink(&log).info(MediaInfo {
        natural_size: Some(size(1920, 1080)),
        has_video: true,
        ..MediaInfo::default()
    });
    sink(&log).state(PlaybackState::Playing);
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    cx.update(|window, cx| window.simulate_next_frame(cx))
}

#[gpui::test]
fn a_playing_picture_drawn_in_the_window_asks_for_the_next_frame(cx: &mut TestAppContext) {
    // One for each time the window drew.
    assert!(frames_asked_for(false, cx) > 0);
}

#[gpui::test]
fn a_session_that_presents_itself_asks_for_no_frames(cx: &mut TestAppContext) {
    assert_eq!(frames_asked_for(true, cx), 0);
}
