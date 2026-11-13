//! The lab's window: a clip list, a backend switcher, the player and a
//! stats panel.
//!
//! Nothing here knows which platform it runs on. The stats sit beside the
//! picture, never over it, because on the web GPUI cannot draw on top of
//! the video.

use std::rc::Rc;
use std::time::Duration;

use gpui::{
    App, AppContext, ClickEvent, Context, Div, Entity, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, Stateful, StatefulInteractiveElement, Styled,
    Subscription, Task, Window, div, px, rgb,
};
use gpui_av::{
    Backend, ColorMatrix, ColorRange, PlaybackState, PlayerEvent, TransferFunction, VideoPlayer,
    video,
};
use web_time::Instant;

use crate::clips::Clip;

/// The playback settings the lab keeps from one clip to the next.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Start every clip muted.
    pub muted: bool,
    /// Loop every clip.
    pub looping: bool,
    /// The volume, from 0.0 to 1.0.
    pub volume: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            muted: false,
            looping: false,
            volume: 1.0,
        }
    }
}

/// The lab's root view.
pub struct LabView {
    backends: Vec<Rc<dyn Backend>>,
    clips: Vec<Clip>,
    backend: usize,
    clip: usize,
    settings: Settings,
    playing: Option<Playing>,
    /// Every time the window has drawn this view, for the bench's window
    /// frame rate.
    renders: u64,
    stage: Stage,
}

/// How the stage shows the picture. The other two arrangements test a
/// presenter that draws outside the window's scene.
#[derive(Clone, Copy, PartialEq)]
enum Stage {
    /// The picture fills the stage.
    Fill,
    /// The picture sits in a scrolling column, so it can be moved and cut.
    Scroll,
    /// The element is gone; the player lives on.
    Hidden,
}

/// The clip being played, with what the lab measures about it.
struct Playing {
    player: Entity<VideoPlayer>,
    meter: Meter,
    _subscriptions: [Subscription; 2],
    /// Redraws the stats while the media moves: a presenter that shows its
    /// frames outside the window asks for no window frames, so nothing
    /// else would.
    _ticker: Task<()>,
}

/// How often the stats are redrawn while the media plays, and how often
/// until the first frame has been seen and timed.
const STATS_TICK: Duration = Duration::from_millis(250);
const FIRST_FRAME_TICK: Duration = Duration::from_millis(8);
/// How long the first frame is watched for that closely.
const FIRST_FRAME_WAIT: Duration = Duration::from_secs(5);

impl LabView {
    /// Creates the view. With `autoplay` it starts the first clip on the
    /// first backend; without, the stage stays empty until a clip is chosen.
    pub fn new(
        backends: Vec<Rc<dyn Backend>>,
        clips: Vec<Clip>,
        settings: Settings,
        autoplay: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            backends,
            clips,
            backend: 0,
            clip: 0,
            settings,
            playing: None,
            renders: 0,
            stage: Stage::Fill,
        };
        if autoplay {
            this.open(cx);
        }
        this
    }

    /// The player for the current clip, if there is a clip.
    pub fn player(&self) -> Option<&Entity<VideoPlayer>> {
        self.playing.as_ref().map(|playing| &playing.player)
    }

    /// Plays clip number `clip` on the current backend.
    pub fn select_clip(&mut self, clip: usize, cx: &mut Context<Self>) {
        if clip < self.clips.len() {
            self.clip = clip;
            self.open(cx);
        }
    }

    /// Plays the current clip again on backend number `backend`.
    pub fn select_backend(&mut self, backend: usize, cx: &mut Context<Self>) {
        if backend < self.backends.len() {
            self.backend = backend;
            self.open(cx);
        }
    }

    /// Plays clip number `clip` on backend number `backend`.
    pub fn select(&mut self, clip: usize, backend: usize, cx: &mut Context<Self>) {
        if clip < self.clips.len() && backend < self.backends.len() {
            (self.clip, self.backend) = (clip, backend);
            self.open(cx);
        }
    }

    /// Stops and drops the current player, leaving the stage empty.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.playing = None;
        cx.notify();
    }

    /// Seeks to `position` and times how long the backend takes to report
    /// the new frame ready (see [`Self::last_seek`]).
    pub fn seek_to(&mut self, position: Duration, cx: &mut Context<Self>) {
        let Some(playing) = &mut self.playing else {
            return;
        };
        playing.meter.seek_started();
        playing.player.update(cx, |player, _| player.seek(position));
    }

    /// How long the last timed seek on the current player took.
    pub fn last_seek(&self) -> Option<Duration> {
        self.playing.as_ref()?.meter.last_seek
    }

    /// How many times the window has drawn this view so far.
    pub fn renders(&self) -> u64 {
        self.renders
    }

    /// Replaces the player with a new one for the current clip and backend.
    fn open(&mut self, cx: &mut Context<Self>) {
        // Dropping the old player first stops its sound before the new one
        // starts.
        self.playing = None;
        let (Some(clip), Some(backend)) =
            (self.clips.get(self.clip), self.backends.get(self.backend))
        else {
            cx.notify();
            return;
        };
        let (source, backend, settings) = (clip.source.clone(), backend.clone(), self.settings);
        let player = cx.new(|cx| {
            let mut player = VideoPlayer::with_backend(source, backend, cx);
            player.set_volume(settings.volume, cx);
            player.set_muted(settings.muted, cx);
            player.set_looping(settings.looping, cx);
            player.play();
            player
        });
        let subscriptions = [
            cx.observe(&player, |_, _, cx| cx.notify()),
            cx.subscribe(&player, |this, _, event: &PlayerEvent, _| {
                if let (Some(playing), PlayerEvent::Seeked(_)) = (&mut this.playing, event) {
                    playing.meter.seek_finished();
                }
            }),
        ];
        let ticker = cx.spawn(async move |this, cx| {
            let mut tick = FIRST_FRAME_TICK;
            loop {
                cx.background_executor().timer(tick).await;
                let next = this.update(cx, |this, cx| {
                    let playing = this.playing.as_ref()?;
                    let state = playing.player.read(cx).state();
                    let moving = matches!(state, PlaybackState::Playing | PlaybackState::Buffering);
                    let waiting = playing.meter.first_frame.is_none()
                        && !matches!(state, PlaybackState::Failed(_))
                        && playing.meter.opened.elapsed() < FIRST_FRAME_WAIT;
                    if moving || waiting {
                        cx.notify();
                    }
                    Some(if waiting {
                        FIRST_FRAME_TICK
                    } else {
                        STATS_TICK
                    })
                });
                match next {
                    Ok(Some(next)) => tick = next,
                    _ => break,
                }
            }
        });
        self.playing = Some(Playing {
            player,
            meter: Meter::new(),
            _subscriptions: subscriptions,
            _ticker: ticker,
        });
        cx.notify();
    }

    fn toggle_play(&mut self, cx: &mut Context<Self>) {
        let Some(player) = self.player() else { return };
        player.update(cx, |player, _| match player.state() {
            PlaybackState::Playing | PlaybackState::Buffering => player.pause(),
            _ => player.play(),
        });
    }

    /// Seeks by `seconds` from the current position, backwards if negative.
    fn seek_by(&mut self, seconds: f64, cx: &mut Context<Self>) {
        let Some(player) = self.player() else { return };
        let target = (player.read(cx).position().as_secs_f64() + seconds).max(0.0);
        self.seek_to(Duration::from_secs_f64(target), cx);
    }

    /// Changes the playback settings, for this player and the next ones.
    pub fn change_settings(&mut self, change: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        change(&mut self.settings);
        let settings = self.settings;
        if let Some(player) = self.player() {
            player.update(cx, |player, cx| {
                player.set_volume(settings.volume, cx);
                player.set_muted(settings.muted, cx);
                player.set_looping(settings.looping, cx);
            });
        }
        cx.notify();
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> Div {
        let backends = self.backends.iter().enumerate().map(|(index, backend)| {
            row(("backend", index), backend.name(), index == self.backend).on_click(
                cx.listener(move |this, _: &ClickEvent, _, cx| this.select_backend(index, cx)),
            )
        });
        let clips = self.clips.iter().enumerate().map(|(index, clip)| {
            row(("clip", index), clip.label.clone(), index == self.clip).on_click(
                cx.listener(move |this, _: &ClickEvent, _, cx| this.select_clip(index, cx)),
            )
        });
        div()
            .w(px(280.))
            .h_full()
            .flex()
            .flex_col()
            .flex_none()
            .gap_1()
            .p_2()
            .bg(rgb(PANEL))
            .child(heading("Backend"))
            .children(backends)
            .child(heading("Clips"))
            .child(
                div()
                    .id("clips")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .flex_1()
                    // A small window still shows a few clips; the stats
                    // give way first.
                    .min_h(px(CLIPS_MIN_HEIGHT))
                    .overflow_y_scroll()
                    .children(clips),
            )
            .child(heading("Stats"))
            .child(
                div()
                    .id("stats")
                    .flex_shrink(1.)
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(self.stats(cx)),
            )
    }

    fn stats(&self, cx: &App) -> Div {
        let Some(playing) = &self.playing else {
            return div().child("No clip. Pass files, folders or URLs on the command line.");
        };
        let player = playing.player.read(cx);
        let info = player.info();
        let stats = player.stats();
        let meter = &playing.meter;
        let or_dash = |value: Option<String>| value.unwrap_or_else(|| "-".to_string());
        let lines = [
            ("backend", player.backend_name().to_string()),
            ("presenter", player.presenter().to_string()),
            ("state", state_label(player.state())),
            (
                "position",
                format!(
                    "{} / {}",
                    clock(player.position()),
                    or_dash(info.duration.map(clock))
                ),
            ),
            (
                "size",
                or_dash(
                    info.natural_size
                        .map(|size| format!("{}x{}", size.width, size.height)),
                ),
            ),
            (
                "rotation",
                info.rotation.map_or("unknown".to_string(), |rotation| {
                    format!("{}°", rotation.degrees())
                }),
            ),
            (
                "tracks",
                match (info.has_video, info.has_audio) {
                    (true, Some(true)) => "video + audio",
                    (true, Some(false)) => "video",
                    (true, None) => "video, audio unknown",
                    (false, Some(true)) => "audio",
                    (false, _) => "-",
                }
                .to_string(),
            ),
            (
                "colour",
                format!(
                    "{}, {}",
                    match info.color.range {
                        Some(ColorRange::Limited) => "limited",
                        Some(ColorRange::Full) => "full",
                        None => "range unknown",
                    },
                    match info.color.matrix {
                        Some(ColorMatrix::Bt601) => "BT.601",
                        Some(ColorMatrix::Bt709) => "BT.709",
                        Some(ColorMatrix::Bt2020) => "BT.2020",
                        Some(_) => "other matrix",
                        None => "matrix unknown",
                    }
                ),
            ),
            (
                // What the media is, not what the screen shows: a backend
                // may draw HDR media in standard range.
                "dynamic range",
                match info.color.transfer {
                    Some(TransferFunction::Sdr) => "SDR",
                    Some(TransferFunction::Pq) => "HDR (PQ)",
                    Some(TransferFunction::Hlg) => "HDR (HLG)",
                    Some(_) => "other",
                    None => "unknown",
                }
                .to_string(),
            ),
            (
                "video fps",
                or_dash(meter.video_fps.map(|fps| format!("{fps:.1}"))),
            ),
            (
                "window fps",
                or_dash(meter.window_fps.map(|fps| format!("{fps:.1}"))),
            ),
            (
                "frames",
                format!(
                    "{} shown, {} dropped",
                    stats.frames_presented, stats.frames_dropped
                ),
            ),
            ("max drift", or_dash(stats.max_drift.map(millis))),
            ("first frame", or_dash(meter.first_frame.map(millis))),
            ("last seek", or_dash(meter.last_seek.map(millis))),
        ];
        let mut panel = div().flex().flex_col().text_xs();
        for (label, value) in lines {
            panel = panel.child(
                div()
                    .flex()
                    .justify_between()
                    .gap_2()
                    .child(div().text_color(rgb(MUTED)).child(label))
                    .child(value),
            );
        }
        if let PlaybackState::Failed(error) = player.state() {
            panel = panel.child(
                div()
                    .mt_1()
                    .text_color(rgb(ERROR))
                    .child(format!("{:?}: {error}", error.kind())),
            );
        }
        panel
    }

    fn transport(&self, cx: &mut Context<Self>) -> Div {
        let paused = self.player().is_none_or(|player| {
            !matches!(
                player.read(cx).state(),
                PlaybackState::Playing | PlaybackState::Buffering
            )
        });
        let settings = self.settings;
        div()
            .flex()
            .flex_wrap()
            .flex_none()
            .items_center()
            .gap_1()
            .p_2()
            .bg(rgb(PANEL))
            .child(
                button("play", if paused { "Play" } else { "Pause" }, false)
                    // The same width either way, so the row does not jump.
                    .w(px(72.))
                    .flex()
                    .justify_center()
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_play(cx))),
            )
            .child(
                button("back", "-5 s", false)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.seek_by(-5., cx))),
            )
            .child(
                button("forward", "+5 s", false)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.seek_by(5., cx))),
            )
            .child(
                button("loop", "Loop", settings.looping).on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        this.change_settings(|settings| settings.looping = !settings.looping, cx);
                    },
                )),
            )
            .child(button("mute", "Mute", settings.muted).on_click(cx.listener(
                |this, _: &ClickEvent, _, cx| {
                    this.change_settings(|settings| settings.muted = !settings.muted, cx);
                },
            )))
            .child(button("quieter", "Vol -", false).on_click(cx.listener(
                |this, _: &ClickEvent, _, cx| {
                    this.change_settings(
                        |settings| settings.volume = (settings.volume - 0.1).max(0.0),
                        cx,
                    );
                },
            )))
            .child(button("louder", "Vol +", false).on_click(cx.listener(
                |this, _: &ClickEvent, _, cx| {
                    this.change_settings(
                        |settings| settings.volume = (settings.volume + 0.1).min(1.0),
                        cx,
                    );
                },
            )))
            .child(
                div()
                    .ml_2()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(format!("volume {:.0}%", settings.volume * 100.)),
            )
            .child(div().flex_1())
            .child(self.stage_button("scroll", "Scroll", Stage::Scroll, cx))
            .child(self.stage_button("hide", "Hide", Stage::Hidden, cx))
    }

    /// A button that switches the stage to `stage`, and back to filling.
    fn stage_button(
        &self,
        id: &'static str,
        label: &'static str,
        stage: Stage,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        button(id, label, self.stage == stage).on_click(cx.listener(
            move |this, _: &ClickEvent, _, cx| {
                this.stage = if this.stage == stage {
                    Stage::Fill
                } else {
                    stage
                };
                cx.notify();
            },
        ))
    }

    /// The area the picture shows in.
    fn stage(&self) -> Stateful<Div> {
        let stage = div().id("stage").flex_1().min_h_0().bg(rgb(0x000000));
        let Some(player) = self.player() else {
            return stage;
        };
        match self.stage {
            Stage::Fill => stage.child(video(player).size_full()),
            Stage::Scroll => {
                let note = |text: &'static str| {
                    div()
                        .h(px(240.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(rgb(MUTED))
                        .child(text)
                };
                stage
                    .overflow_y_scroll()
                    .child(note("Scroll: the picture moves with this text."))
                    .child(video(player).w_full().h(px(480.)))
                    .child(note("The stage's edges cut the picture."))
                    .child(div().h(px(480.)))
            }
            Stage::Hidden => stage.child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(rgb(MUTED))
                    .child("The picture is hidden; the player keeps going."),
            ),
        }
    }
}

impl Render for LabView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.renders += 1;
        if let Some(playing) = &mut self.playing {
            let player = playing.player.read(cx);
            let presented = player.stats().frames_presented;
            if matches!(
                player.state(),
                PlaybackState::Playing | PlaybackState::Buffering
            ) {
                playing.meter.rendered(presented);
            } else {
                // Renders are rare while nothing moves; rates measured
                // across a pause would be meaningless.
                playing.meter.idle(presented);
            }
        }
        let stage = self.stage();
        div()
            .size_full()
            .flex()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(TEXT))
            .text_sm()
            .child(self.sidebar(cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(stage)
                    .child(self.transport(cx)),
            )
    }
}

/// About three rows of the clip list.
const CLIPS_MIN_HEIGHT: f32 = 96.;
const BACKGROUND: u32 = 0x16181d;
const PANEL: u32 = 0x1f2229;
const SELECTED: u32 = 0x2f5fd0;
const HOVER: u32 = 0x2a2e37;
const TEXT: u32 = 0xe6e8ec;
const MUTED: u32 = 0x9097a3;
const ERROR: u32 = 0xff7b72;

fn heading(text: &'static str) -> Div {
    div()
        .mt_2()
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .text_color(rgb(MUTED))
        .child(text)
}

fn row(id: (&'static str, usize), label: impl Into<SharedString>, selected: bool) -> Stateful<Div> {
    div()
        .id(id)
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .when_selected(selected)
        .child(label.into())
}

fn button(id: &'static str, label: &'static str, selected: bool) -> Stateful<Div> {
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .bg(rgb(HOVER))
        .when_selected(selected)
        .child(label)
}

trait WhenSelected {
    fn when_selected(self, selected: bool) -> Self;
}

impl WhenSelected for Stateful<Div> {
    fn when_selected(self, selected: bool) -> Self {
        if selected {
            self.bg(rgb(SELECTED))
        } else {
            self.hover(|style| style.bg(rgb(HOVER)))
        }
    }
}

/// What the lab times for one player: how long the first frame took, how
/// long the last seek took, and frame rates over the last second.
struct Meter {
    opened: Instant,
    first_frame: Option<Duration>,
    seek_started: Option<Instant>,
    last_seek: Option<Duration>,
    window: RateWindow,
    video_fps: Option<f64>,
    window_fps: Option<f64>,
}

/// Counts over the current one-second window.
struct RateWindow {
    since: Instant,
    renders: u32,
    presented_at_start: u64,
}

impl Meter {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            opened: now,
            first_frame: None,
            seek_started: None,
            last_seek: None,
            window: RateWindow {
                since: now,
                renders: 0,
                presented_at_start: 0,
            },
            video_fps: None,
            window_fps: None,
        }
    }

    /// Called on every render with the player's count of frames presented.
    fn rendered(&mut self, presented: u64) {
        if self.first_frame.is_none() && presented > 0 {
            self.first_frame = Some(self.opened.elapsed());
        }
        self.window.renders += 1;
        let elapsed = self.window.since.elapsed();
        if elapsed >= Duration::from_secs(1) {
            let seconds = elapsed.as_secs_f64();
            let frames = presented.saturating_sub(self.window.presented_at_start);
            self.video_fps = Some(frames as f64 / seconds);
            self.window_fps = Some(f64::from(self.window.renders) / seconds);
            self.window = RateWindow {
                since: Instant::now(),
                renders: 0,
                presented_at_start: presented,
            };
        }
    }

    /// Forgets the frame rates and starts a new window from now: called
    /// while the media is not moving.
    fn idle(&mut self, presented: u64) {
        if self.first_frame.is_none() && presented > 0 {
            self.first_frame = Some(self.opened.elapsed());
        }
        self.video_fps = None;
        self.window_fps = None;
        self.window = RateWindow {
            since: Instant::now(),
            renders: 0,
            presented_at_start: presented,
        };
    }

    fn seek_started(&mut self) {
        self.seek_started = Some(Instant::now());
        self.last_seek = None;
    }

    fn seek_finished(&mut self) {
        if let Some(started) = self.seek_started.take() {
            self.last_seek = Some(started.elapsed());
        }
    }
}

fn state_label(state: &PlaybackState) -> String {
    match state {
        PlaybackState::Loading => "loading",
        PlaybackState::Ready => "ready",
        PlaybackState::Playing => "playing",
        PlaybackState::Paused => "paused",
        PlaybackState::Buffering => "buffering",
        PlaybackState::Ended => "ended",
        PlaybackState::Failed(_) => "failed",
    }
    .to_string()
}

/// A position as minutes, seconds and tenths: `1:07.3`.
fn clock(time: Duration) -> String {
    let tenths = time.as_millis() / 100;
    format!("{}:{:02}.{}", tenths / 600, tenths / 10 % 60, tenths % 10)
}

fn millis(time: Duration) -> String {
    format!("{} ms", time.as_millis())
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, VisualTestContext};
    use gpui_av::Source;

    use super::*;
    use crate::test_pattern::TestPatternBackend;

    #[test]
    fn a_position_reads_as_minutes_seconds_tenths() {
        assert_eq!(clock(Duration::ZERO), "0:00.0");
        assert_eq!(clock(Duration::from_millis(67_349)), "1:07.3");
        assert_eq!(clock(Duration::from_secs(600)), "10:00.0");
    }

    fn pattern_clips() -> Vec<Clip> {
        ["pattern://640x360?fps=30&duration=5", "pattern://bad"]
            .into_iter()
            .map(|url| Clip {
                label: url.to_string(),
                source: Source::url(url),
            })
            .collect()
    }

    fn lab(cx: &mut TestAppContext) -> (Entity<LabView>, &mut VisualTestContext) {
        let backends: Vec<Rc<dyn Backend>> =
            vec![Rc::new(TestPatternBackend), Rc::new(TestPatternBackend)];
        cx.add_window_view(|_, cx| {
            LabView::new(backends, pattern_clips(), Settings::default(), true, cx)
        })
    }

    fn state(view: &Entity<LabView>, cx: &mut VisualTestContext) -> PlaybackState {
        view.read_with(cx, |view, cx| {
            view.player().expect("a player").read(cx).state().clone()
        })
    }

    #[gpui::test]
    fn the_first_clip_plays_on_the_first_backend(cx: &mut TestAppContext) {
        let (view, cx) = lab(cx);
        cx.run_until_parked();

        assert_eq!(state(&view, cx), PlaybackState::Playing);
        view.read_with(cx, |view, cx| {
            let player = view.player().unwrap().read(cx);
            assert_eq!(player.backend_name(), "test-pattern");
            assert_eq!(player.presenter(), "image");
            assert_eq!(player.duration(), Some(Duration::from_secs(5)));
            // The window was drawn, so the pattern painted a frame.
            assert!(player.stats().frames_presented >= 1);
        });
    }

    #[gpui::test]
    fn switching_backend_or_clip_opens_a_new_player(cx: &mut TestAppContext) {
        let (view, cx) = lab(cx);
        cx.run_until_parked();
        let first = view.read_with(cx, |view, _| view.player().unwrap().entity_id());

        view.update(cx, |view, cx| view.select_backend(1, cx));
        cx.run_until_parked();
        let second = view.read_with(cx, |view, _| view.player().unwrap().entity_id());
        assert_ne!(first, second);
        assert_eq!(state(&view, cx), PlaybackState::Playing);

        // The second clip is one the backend refuses.
        view.update(cx, |view, cx| view.select_clip(1, cx));
        cx.run_until_parked();
        assert!(matches!(state(&view, cx), PlaybackState::Failed(_)));
    }

    #[gpui::test]
    fn the_transport_drives_the_player(cx: &mut TestAppContext) {
        let (view, cx) = lab(cx);
        cx.run_until_parked();

        view.update(cx, |view, cx| view.toggle_play(cx));
        cx.run_until_parked();
        assert_eq!(state(&view, cx), PlaybackState::Paused);

        view.update(cx, |view, cx| {
            view.seek_by(2., cx);
            view.change_settings(|settings| settings.looping = true, cx);
        });
        cx.run_until_parked();
        view.read_with(cx, |view, cx| {
            let player = view.player().unwrap().read(cx);
            assert!(player.position() >= Duration::from_secs(2));
            assert!(player.looping());
            assert!(view.playing.as_ref().unwrap().meter.last_seek.is_some());
        });
    }
}
