//! `Fallback` tries backends in order, using only the public API.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{App, AppContext, Entity, TestAppContext, Window, size};
use gpui_av::{
    Backend, Error, ErrorKind, EventSink, Fallback, MediaInfo, Placement, PlaybackState,
    PlayerEvent, Session, Source, VideoPlayer,
};

/// What one scripted backend saw.
#[derive(Default)]
struct Log {
    opened: usize,
    commands: Vec<String>,
    sink: Option<EventSink>,
    dropped: usize,
}

struct Scripted {
    name: &'static str,
    log: Rc<RefCell<Log>>,
    refuse: Option<Error>,
}

impl Backend for Scripted {
    fn name(&self) -> &'static str {
        self.name
    }

    fn presenter(&self) -> &'static str {
        "image"
    }

    fn open(&self, _: Source, events: EventSink, _: &mut App) -> Result<Box<dyn Session>, Error> {
        let mut log = self.log.borrow_mut();
        log.opened += 1;
        if let Some(error) = &self.refuse {
            return Err(error.clone());
        }
        log.sink = Some(events);
        Ok(Box::new(ScriptedSession(self.log.clone())))
    }
}

struct ScriptedSession(Rc<RefCell<Log>>);

impl ScriptedSession {
    fn record(&self, command: impl Into<String>) {
        self.0.borrow_mut().commands.push(command.into());
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
    fn select_track(&mut self, kind: gpui_av::TrackKind, index: Option<usize>) {
        self.record(format!("track {kind:?} {index:?}"));
    }
    fn position(&self) -> Duration {
        Duration::ZERO
    }
    fn paint(&mut self, _: Placement, _: &mut Window, _: &mut App) {}
}

impl Drop for ScriptedSession {
    fn drop(&mut self) {
        self.0.borrow_mut().dropped += 1;
    }
}

fn backend(name: &'static str, refuse: Option<Error>) -> (Rc<dyn Backend>, Rc<RefCell<Log>>) {
    let log = Rc::new(RefCell::new(Log::default()));
    let backend = Rc::new(Scripted {
        name,
        log: log.clone(),
        refuse,
    });
    (backend, log)
}

fn unsupported(message: &str) -> Error {
    Error::new(ErrorKind::Unsupported, message)
}

fn sink(log: &Rc<RefCell<Log>>) -> EventSink {
    log.borrow().sink.clone().expect("the backend was opened")
}

fn info(width: u32) -> MediaInfo {
    MediaInfo {
        duration: Some(Duration::from_secs(10)),
        natural_size: Some(size(width, 720)),
        has_video: true,
        ..MediaInfo::default()
    }
}

fn player(backends: Vec<Rc<dyn Backend>>, cx: &mut TestAppContext) -> Entity<VideoPlayer> {
    let fallback: Rc<dyn Backend> = Rc::new(Fallback::new(backends));
    cx.new(|cx| VideoPlayer::with_backend(Source::file("/clip.mkv"), fallback, cx))
}

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

#[gpui::test]
fn a_refusal_at_open_passes_to_the_next_backend(cx: &mut TestAppContext) {
    let (first, first_log) = backend("first", Some(unsupported("no MKV")));
    let (second, second_log) = backend("second", None);
    let player = player(vec![first, second], cx);

    assert_eq!(first_log.borrow().opened, 1);
    assert_eq!(second_log.borrow().opened, 1);
    sink(&second_log).state(PlaybackState::Ready);
    cx.run_until_parked();
    player.read_with(cx, |player, _| {
        assert_eq!(*player.state(), PlaybackState::Ready);
        assert_eq!(player.backend_name(), "second");
    });
}

#[gpui::test]
fn a_refusal_while_loading_passes_on_and_replays_commands(cx: &mut TestAppContext) {
    let (first, first_log) = backend("first", None);
    let (second, second_log) = backend("second", None);
    let player = player(vec![first, second], cx);
    let events = record_events(&player, cx);

    player.update(cx, |player, cx| {
        player.set_muted(true, cx);
        player.select_audio_track(1);
        player.seek(Duration::from_secs(2));
        player.play();
    });
    sink(&first_log).info(info(640));
    sink(&first_log).fail(unsupported("FileFormatNotRecognized"));
    cx.run_until_parked();

    assert_eq!(first_log.borrow().dropped, 1, "the first session is let go");
    assert_eq!(
        second_log.borrow().commands,
        ["muted true", "track Audio Some(1)", "seek 2000", "play"]
    );
    player.read_with(cx, |player, _| {
        assert_eq!(*player.state(), PlaybackState::Loading);
        assert_eq!(player.backend_name(), "second");
        assert_eq!(
            player.natural_size(),
            None,
            "the first backend's info is not shown"
        );
    });

    sink(&second_log).info(info(1280));
    sink(&second_log).state(PlaybackState::Playing);
    cx.run_until_parked();
    player.read_with(cx, |player, _| {
        assert_eq!(*player.state(), PlaybackState::Playing);
        assert_eq!(player.natural_size(), Some(size(1280, 720)));
    });
    assert_eq!(
        *events.borrow(),
        [
            PlayerEvent::InfoChanged,
            PlayerEvent::StateChanged(PlaybackState::Playing),
        ]
    );
}

#[gpui::test]
fn other_failures_are_final(cx: &mut TestAppContext) {
    let (first, first_log) = backend("first", None);
    let (second, second_log) = backend("second", None);
    let player = player(vec![first, second], cx);
    let refused = Error::new(ErrorKind::Unauthorized, "HTTP 401");

    sink(&first_log).fail(refused.clone());
    cx.run_until_parked();

    assert_eq!(second_log.borrow().opened, 0);
    player.read_with(cx, |player, _| {
        assert_eq!(*player.state(), PlaybackState::Failed(refused));
        assert_eq!(player.backend_name(), "first");
    });
}

#[gpui::test]
fn a_backend_with_the_media_ready_keeps_it(cx: &mut TestAppContext) {
    let (first, first_log) = backend("first", None);
    let (second, second_log) = backend("second", None);
    let player = player(vec![first, second], cx);
    let late = unsupported("a codec change halfway");

    sink(&first_log).state(PlaybackState::Ready);
    sink(&first_log).fail(late.clone());
    cx.run_until_parked();

    assert_eq!(second_log.borrow().opened, 0);
    player.read_with(cx, |player, _| {
        assert_eq!(*player.state(), PlaybackState::Failed(late));
    });
}

#[gpui::test]
fn when_every_backend_refuses_the_error_names_them_all(cx: &mut TestAppContext) {
    let (first, _) = backend("first", Some(unsupported("no MKV")));
    let (second, second_log) = backend("second", None);
    let player = player(vec![first, second], cx);
    let events = record_events(&player, cx);

    sink(&second_log).fail(unsupported("no plugin for VP9"));
    cx.run_until_parked();

    player.read_with(cx, |player, _| match player.state() {
        PlaybackState::Failed(error) => {
            assert_eq!(error.kind(), ErrorKind::Unsupported);
            assert!(error.message().contains("first: no MKV"), "{error}");
            assert!(
                error.message().contains("second: no plugin for VP9"),
                "{error}"
            );
        }
        other => panic!("expected a failure, got {other:?}"),
    });
    assert_eq!(events.borrow().len(), 1);
}

#[gpui::test]
fn when_every_backend_refuses_at_open_the_player_fails_at_once(cx: &mut TestAppContext) {
    let (first, _) = backend("first", Some(unsupported("no MKV")));
    let (second, _) = backend("second", Some(unsupported("not installed")));
    let player = player(vec![first, second], cx);

    player.read_with(cx, |player, _| match player.state() {
        PlaybackState::Failed(error) => {
            assert!(error.message().contains("second: not installed"), "{error}");
        }
        other => panic!("expected a failure, got {other:?}"),
    });
}

#[gpui::test]
fn events_from_a_backend_passed_over_are_ignored(cx: &mut TestAppContext) {
    let (first, first_log) = backend("first", None);
    let (second, _) = backend("second", None);
    let player = player(vec![first, second], cx);
    let stale = sink(&first_log);

    stale.fail(unsupported("no MKV"));
    cx.run_until_parked();
    stale.state(PlaybackState::Playing);
    cx.run_until_parked();

    player.read_with(cx, |player, _| {
        assert_eq!(*player.state(), PlaybackState::Loading);
    });
}
