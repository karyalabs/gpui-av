//! A backend with no decoder: it draws a synthetic picture against the wall
//! clock.
//!
//! It exists to prove that a backend can live outside the gpui-av crates.
//! Everything here uses the public API only (`gpui_av::Backend`,
//! `gpui_av::Session` and GPUI), so any app could ship the same code. It
//! also gives the lab something to play on a machine with no media at all.
//!
//! A source of the form `pattern://1920x1080?fps=60&duration=20` sets the
//! picture's size, frame rate and length; any other source plays the
//! default pattern. There is no sound.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{App, Corners, RenderImage, Size, Task, Window, size};
use gpui_av::{
    Backend, Error, ErrorKind, EventSink, MediaInfo, Placement, PlaybackState, Session, Source,
    Stats,
};
use image::{Frame, RgbaImage};
use smallvec::SmallVec;
use web_time::Instant;

/// The pattern is drawn this wide at most and stretched to its nominal
/// size, so a frame costs the same whatever size the source claims.
const MAX_RENDER_WIDTH: u32 = 320;

/// How often the session looks at the clock when nothing is painting it.
const TICK: Duration = Duration::from_millis(50);

/// The synthetic backend.
pub struct TestPatternBackend;

impl Backend for TestPatternBackend {
    fn name(&self) -> &'static str {
        "test-pattern"
    }

    fn presenter(&self) -> &'static str {
        "image"
    }

    fn open(
        &self,
        source: Source,
        events: EventSink,
        cx: &mut App,
    ) -> Result<Box<dyn Session>, Error> {
        let pattern = Pattern::from_source(&source)?;
        events.info(MediaInfo {
            duration: Some(pattern.duration),
            natural_size: Some(pattern.size),
            has_video: true,
            ..MediaInfo::default()
        });
        events.state(PlaybackState::Ready);

        let epoch = Instant::now();
        let shared = Rc::new(RefCell::new(Shared {
            timeline: Timeline::new(pattern.duration),
            events,
        }));
        // Notices the end of the media while no window is painting it.
        let ticker = cx.spawn({
            let shared = Rc::downgrade(&shared);
            async move |cx| {
                loop {
                    cx.background_executor().timer(TICK).await;
                    let Some(shared) = shared.upgrade() else {
                        break;
                    };
                    shared.borrow_mut().advance(epoch.elapsed());
                }
            }
        });
        Ok(Box::new(PatternSession {
            pattern,
            epoch,
            shared,
            shown: None,
            stats: Stats::default(),
            _ticker: ticker,
        }))
    }
}

/// What to draw: read from the source.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Pattern {
    size: Size<u32>,
    fps: u32,
    duration: Duration,
}

impl Default for Pattern {
    fn default() -> Self {
        Self {
            size: size(1280, 720),
            fps: 60,
            duration: Duration::from_secs(10),
        }
    }
}

impl Pattern {
    fn from_source(source: &Source) -> Result<Self, Error> {
        let Some(spec) = source
            .as_url()
            .and_then(|url| url.strip_prefix("pattern://"))
        else {
            return Ok(Self::default());
        };
        let bad = |what: &str| {
            Error::new(
                ErrorKind::Unsupported,
                format!("{what} in pattern source \"pattern://{spec}\""),
            )
        };
        let (dimensions, query) = spec.split_once('?').unwrap_or((spec, ""));
        let mut pattern = Self::default();
        if !dimensions.is_empty() {
            let (width, height) = dimensions
                .split_once('x')
                .and_then(|(width, height)| Some((width.parse().ok()?, height.parse().ok()?)))
                .filter(|(width, height)| *width > 0 && *height > 0)
                .ok_or_else(|| bad("a size that is not WIDTHxHEIGHT"))?;
            pattern.size = size(width, height);
        }
        for pair in query.split('&').filter(|pair| !pair.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            match key {
                "fps" => {
                    pattern.fps = value
                        .parse()
                        .ok()
                        .filter(|fps| (1..=240).contains(fps))
                        .ok_or_else(|| bad("an fps outside 1 to 240"))?;
                }
                "duration" => {
                    pattern.duration = value
                        .parse::<f64>()
                        .ok()
                        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
                        .map(Duration::from_secs_f64)
                        .ok_or_else(|| bad("a duration that is not a number of seconds"))?;
                }
                _ => return Err(bad("an unknown option")),
            }
        }
        Ok(pattern)
    }

    /// The frame showing at `position`.
    fn frame_at(&self, position: Duration) -> u64 {
        let last = (self.duration.as_secs_f64() * f64::from(self.fps)).ceil() as u64;
        let frame = (position.as_secs_f64() * f64::from(self.fps)) as u64;
        frame.min(last.saturating_sub(1))
    }

    fn render_size(&self) -> Size<u32> {
        let width = self.size.width.min(MAX_RENDER_WIDTH);
        let height = (u64::from(self.size.height) * u64::from(width) / u64::from(self.size.width))
            .max(1) as u32;
        size(width, height)
    }

    /// Draws frame number `frame`: colour bars, a white line that sweeps
    /// across once a second, a square that flashes on the first frames of
    /// each second, and the frame number in binary along the bottom. The
    /// pixels are BGRA, which is what GPUI's images hold.
    fn render(&self, frame: u64) -> RgbaImage {
        const BARS: [[u8; 3]; 8] = [
            [192, 192, 192],
            [192, 192, 0],
            [0, 192, 192],
            [0, 192, 0],
            [192, 0, 192],
            [192, 0, 0],
            [0, 0, 192],
            [16, 16, 16],
        ];
        let Size { width, height } = self.render_size();
        let fps = u64::from(self.fps);
        let in_second = frame % fps;
        let sweep = (in_second * u64::from(width) / fps) as u32;
        let flash = in_second < (fps / 15).max(1);
        let bits_top = height - (height / 8).max(1);
        let bit_width = (width / 16).max(1);
        let flash_side = (height / 4).max(1);

        let mut pixels = vec![0u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let [r, g, b] = if y >= bits_top {
                    let bit = 15 - (x / bit_width).min(15);
                    if frame >> bit & 1 == 1 {
                        [255, 255, 255]
                    } else {
                        [0, 0, 0]
                    }
                } else if x.abs_diff(sweep) < 2 || (flash && x < flash_side && y < flash_side) {
                    [255, 255, 255]
                } else {
                    BARS[(x * 8 / width).min(7) as usize]
                };
                let at = ((y * width + x) * 4) as usize;
                pixels[at..at + 4].copy_from_slice(&[b, g, r, 255]);
            }
        }
        RgbaImage::from_raw(width, height, pixels).expect("the buffer matches the size")
    }
}

/// The session's clock and transport state, as a function of the time since
/// the session was opened. It holds no clock of its own, so it can be
/// tested with made-up times.
#[derive(Debug)]
struct Timeline {
    duration: Duration,
    /// The position when the clock last started or stopped.
    base: Duration,
    /// When the clock started, while playing.
    playing_since: Option<Duration>,
    looping: bool,
    ended: bool,
    /// Set when the position jumped (a seek, a loop, a restart), so the
    /// gap in frame numbers is not counted as dropped frames.
    jumped: bool,
}

/// What a [`Timeline`] step asks the session to report.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Report {
    State(PlaybackStateKind),
    Seeked(Duration),
}

/// The states this backend moves between, without their payload.
#[derive(Clone, Copy, Debug, PartialEq)]
enum PlaybackStateKind {
    Playing,
    Paused,
    Ended,
}

impl Timeline {
    fn new(duration: Duration) -> Self {
        Self {
            duration,
            base: Duration::ZERO,
            playing_since: None,
            looping: false,
            ended: false,
            jumped: true,
        }
    }

    /// The position at `now`, before looping or ending is applied.
    fn raw_position(&self, now: Duration) -> Duration {
        match self.playing_since {
            Some(since) => self.base + now.saturating_sub(since),
            None => self.base,
        }
    }

    fn position(&self, now: Duration) -> Duration {
        self.raw_position(now).min(self.duration)
    }

    /// Applies the end of the media: loops or stops.
    fn advance(&mut self, now: Duration) -> Option<Report> {
        let raw = self.raw_position(now);
        if self.playing_since.is_none() || raw < self.duration {
            return None;
        }
        if self.looping {
            let nanos = raw.as_nanos() % self.duration.as_nanos().max(1);
            self.base = Duration::from_nanos(nanos as u64);
            self.playing_since = Some(now);
            self.jumped = true;
            None
        } else {
            self.base = self.duration;
            self.playing_since = None;
            self.ended = true;
            Some(Report::State(PlaybackStateKind::Ended))
        }
    }

    fn play(&mut self, now: Duration) -> Option<Report> {
        if self.playing_since.is_some() {
            return None;
        }
        if self.ended {
            self.base = Duration::ZERO;
            self.ended = false;
            self.jumped = true;
        }
        self.playing_since = Some(now);
        Some(Report::State(PlaybackStateKind::Playing))
    }

    fn pause(&mut self, now: Duration) -> Option<Report> {
        self.playing_since?;
        self.base = self.position(now);
        self.playing_since = None;
        Some(Report::State(PlaybackStateKind::Paused))
    }

    fn seek(&mut self, position: Duration, now: Duration) -> [Option<Report>; 2] {
        self.base = position.min(self.duration);
        self.jumped = true;
        if self.playing_since.is_some() {
            self.playing_since = Some(now);
        }
        // Seeking away from the end leaves the media paused, not ended.
        let state =
            std::mem::take(&mut self.ended).then_some(Report::State(PlaybackStateKind::Paused));
        [state, Some(Report::Seeked(self.base))]
    }
}

/// What the session and its ticker share.
struct Shared {
    timeline: Timeline,
    events: EventSink,
}

impl Shared {
    fn report(&self, report: Option<Report>) {
        match report {
            Some(Report::State(PlaybackStateKind::Playing)) => {
                self.events.state(PlaybackState::Playing);
            }
            Some(Report::State(PlaybackStateKind::Paused)) => {
                self.events.state(PlaybackState::Paused);
            }
            Some(Report::State(PlaybackStateKind::Ended)) => {
                self.events.state(PlaybackState::Ended);
            }
            Some(Report::Seeked(position)) => self.events.seeked(position),
            None => {}
        }
    }

    fn advance(&mut self, now: Duration) {
        let report = self.timeline.advance(now);
        self.report(report);
    }
}

struct PatternSession {
    pattern: Pattern,
    epoch: Instant,
    shared: Rc<RefCell<Shared>>,
    /// The frame in the window now, and its image.
    shown: Option<(u64, Arc<RenderImage>)>,
    stats: Stats,
    _ticker: Task<()>,
}

impl PatternSession {
    fn now(&self) -> Duration {
        self.epoch.elapsed()
    }
}

impl Session for PatternSession {
    fn play(&mut self) {
        let now = self.now();
        let mut shared = self.shared.borrow_mut();
        let report = shared.timeline.play(now);
        shared.report(report);
    }

    fn pause(&mut self) {
        let now = self.now();
        let mut shared = self.shared.borrow_mut();
        let report = shared.timeline.pause(now);
        shared.report(report);
    }

    fn seek(&mut self, position: Duration) {
        let now = self.now();
        let mut shared = self.shared.borrow_mut();
        for report in shared.timeline.seek(position, now) {
            shared.report(report);
        }
    }

    // There is no sound to turn up or down.
    fn set_volume(&mut self, _volume: f32) {}

    fn set_muted(&mut self, _muted: bool) {}

    fn set_looping(&mut self, looping: bool) {
        self.shared.borrow_mut().timeline.looping = looping;
    }

    fn position(&self) -> Duration {
        self.shared.borrow().timeline.position(self.now())
    }

    fn stats(&self) -> Stats {
        self.stats
    }

    fn paint(&mut self, placement: Placement, window: &mut Window, _cx: &mut App) {
        let now = self.now();
        let (frame, jumped) = {
            let mut shared = self.shared.borrow_mut();
            shared.advance(now);
            let frame = self.pattern.frame_at(shared.timeline.position(now));
            (frame, std::mem::take(&mut shared.timeline.jumped))
        };
        let previous = self.shown.as_ref().map(|(shown, _)| *shown);
        if previous != Some(frame) {
            let image = Arc::new(RenderImage::new(SmallVec::from_elem(
                Frame::new(self.pattern.render(frame)),
                1,
            )));
            if let Some((_, old)) = self.shown.replace((frame, image)) {
                // Frees the old frame's texture; without this every frame
                // would stay in GPUI's atlas.
                let _ = window.drop_image(old);
            }
            let skipped = match previous {
                Some(previous) if !jumped && frame > previous => frame - previous - 1,
                _ => 0,
            };
            self.stats = Stats::new(
                self.stats.frames_presented + 1,
                self.stats.frames_dropped + skipped,
            );
        }
        if let Some((_, image)) = &self.shown {
            let _ = window.paint_image(
                placement.bounds,
                placement.bounds,
                Corners::default(),
                image.clone(),
                0,
                false,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn secs(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    #[test]
    fn a_pattern_source_sets_size_rate_and_length() {
        let source = Source::url("pattern://1920x1080?fps=30&duration=2.5");
        assert_eq!(
            Pattern::from_source(&source).unwrap(),
            Pattern {
                size: size(1920, 1080),
                fps: 30,
                duration: Duration::from_millis(2500),
            }
        );
    }

    #[test]
    fn any_other_source_plays_the_default_pattern() {
        for source in [Source::file("/clips/a.mp4"), Source::url("pattern://")] {
            assert_eq!(Pattern::from_source(&source).unwrap(), Pattern::default());
        }
    }

    #[test]
    fn a_malformed_pattern_source_is_refused() {
        for url in [
            "pattern://wide",
            "pattern://0x720",
            "pattern://640x360?fps=0",
            "pattern://640x360?duration=-1",
            "pattern://640x360?speed=2",
        ] {
            let error = Pattern::from_source(&Source::url(url)).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Unsupported, "{url}");
        }
    }

    #[test]
    fn frames_follow_the_position_and_stop_at_the_last_one() {
        let pattern = Pattern {
            fps: 30,
            duration: secs(2),
            ..Pattern::default()
        };
        assert_eq!(pattern.frame_at(Duration::ZERO), 0);
        assert_eq!(pattern.frame_at(Duration::from_millis(500)), 15);
        assert_eq!(pattern.frame_at(secs(2)), 59);
    }

    #[test]
    fn a_frame_is_drawn_small_and_keeps_its_shape() {
        let pattern = Pattern {
            size: size(1920, 1080),
            ..Pattern::default()
        };
        let image = pattern.render(7);
        assert_eq!((image.width(), image.height()), (320, 180));
        // Frame 7 is binary 111: the three rightmost blocks are white.
        let bottom = image.height() - 1;
        assert_eq!(image.get_pixel(319, bottom).0, [255, 255, 255, 255]);
        assert_eq!(image.get_pixel(319 - 3 * 20, bottom).0, [0, 0, 0, 255]);
    }

    #[test]
    fn the_clock_runs_only_while_playing() {
        let mut timeline = Timeline::new(secs(10));
        assert_eq!(timeline.position(secs(5)), Duration::ZERO);

        assert_eq!(
            timeline.play(secs(5)),
            Some(Report::State(PlaybackStateKind::Playing))
        );
        assert_eq!(timeline.play(secs(6)), None);
        assert_eq!(timeline.position(secs(8)), secs(3));

        assert_eq!(
            timeline.pause(secs(8)),
            Some(Report::State(PlaybackStateKind::Paused))
        );
        assert_eq!(timeline.pause(secs(9)), None);
        assert_eq!(timeline.position(secs(60)), secs(3));
    }

    #[test]
    fn playing_past_the_end_ends_and_play_starts_over() {
        let mut timeline = Timeline::new(secs(10));
        timeline.play(secs(0));
        assert_eq!(timeline.advance(secs(9)), None);
        assert_eq!(
            timeline.advance(secs(11)),
            Some(Report::State(PlaybackStateKind::Ended))
        );
        assert_eq!(timeline.position(secs(20)), secs(10));

        timeline.play(secs(20));
        assert_eq!(timeline.position(secs(21)), secs(1));
    }

    #[test]
    fn looping_wraps_instead_of_ending() {
        let mut timeline = Timeline::new(secs(10));
        timeline.looping = true;
        timeline.play(secs(0));
        timeline.jumped = false;

        assert_eq!(timeline.advance(secs(12)), None);
        assert_eq!(timeline.position(secs(12)), secs(2));
        assert!(timeline.jumped);
    }

    #[test]
    fn a_seek_moves_the_clock_and_reports() {
        let mut timeline = Timeline::new(secs(10));
        timeline.play(secs(0));
        assert_eq!(
            timeline.seek(secs(4), secs(1)),
            [None, Some(Report::Seeked(secs(4)))]
        );
        assert_eq!(timeline.position(secs(3)), secs(6));
        // Past the end is clamped.
        assert_eq!(
            timeline.seek(secs(99), secs(3)),
            [None, Some(Report::Seeked(secs(10)))]
        );
    }

    #[test]
    fn a_seek_after_the_end_leaves_the_media_paused() {
        let mut timeline = Timeline::new(secs(10));
        timeline.play(secs(0));
        timeline.advance(secs(10));
        assert_eq!(
            timeline.seek(secs(2), secs(11)),
            [
                Some(Report::State(PlaybackStateKind::Paused)),
                Some(Report::Seeked(secs(2)))
            ]
        );
        assert_eq!(timeline.position(secs(30)), secs(2));
    }
}
