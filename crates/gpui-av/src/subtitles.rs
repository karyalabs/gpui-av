//! Subtitles from a sidecar file (WebVTT or SubRip), and an element that
//! shows the cue due at the player's position.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, Div, Entity, IntoElement, ParentElement, RenderOnce, SharedString, StyleRefinement,
    Styled, Window, div, px, rgba,
};

use crate::VideoPlayer;

/// One subtitle: text shown from `start` until `end`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cue {
    /// When the text appears.
    pub start: Duration,
    /// When it goes.
    pub end: Duration,
    /// The text, with markup such as `<i>` removed; lines are separated by
    /// `\n`.
    pub text: SharedString,
}

/// A track of subtitles parsed from a WebVTT (`.vtt`) or SubRip (`.srt`)
/// file. Cheap to clone.
///
/// ```no_run
/// # use gpui::{Entity, IntoElement, ParentElement, Styled, div};
/// # use gpui_av::{Subtitles, VideoPlayer, captions, video};
/// # fn render(player: &Entity<VideoPlayer>) -> Result<impl IntoElement, Box<dyn std::error::Error>> {
/// let subtitles = Subtitles::parse(&std::fs::read_to_string("film.vtt")?)?;
/// // In render, over the picture:
/// Ok(div()
///     .relative()
///     .child(video(player).size_full())
///     .child(captions(player).subtitles(&subtitles)))
/// # }
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Subtitles {
    cues: Arc<[Cue]>,
}

/// Why a subtitle file could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubtitleError {
    line: usize,
    message: &'static str,
}

impl SubtitleError {
    /// The line the problem is on, counted from 1.
    pub fn line(&self) -> usize {
        self.line
    }
}

impl fmt::Display for SubtitleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for SubtitleError {}

impl Subtitles {
    /// Parses WebVTT or SubRip, telling them apart by WebVTT's `WEBVTT`
    /// header. A cue whose timing line cannot be read is an error; cue
    /// settings, styles, regions and notes are ignored.
    pub fn parse(text: &str) -> Result<Self, SubtitleError> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let webvtt = text.starts_with("WEBVTT");
        let mut cues = Vec::new();
        let lines: Vec<&str> = text.lines().collect();
        let mut index = 0;
        if webvtt {
            // The header block ends at the first blank line.
            while index < lines.len() && !lines[index].trim().is_empty() {
                index += 1;
            }
        }
        while index < lines.len() {
            // Skip blank lines between blocks.
            while index < lines.len() && lines[index].trim().is_empty() {
                index += 1;
            }
            let start = index;
            while index < lines.len() && !lines[index].trim().is_empty() {
                index += 1;
            }
            let block = &lines[start..index];
            if block.is_empty() {
                continue;
            }
            if webvtt && is_webvtt_metadata(block[0]) {
                continue;
            }
            // An identifier line (a SubRip number, a WebVTT name) may come
            // before the timing.
            let Some(timing) = block.iter().position(|line| line.contains("-->")) else {
                return Err(SubtitleError {
                    line: start + 1,
                    message: "a cue without a timing line",
                });
            };
            if timing > 1 {
                return Err(SubtitleError {
                    line: start + 1,
                    message: "a cue without a timing line",
                });
            }
            let line = start + timing + 1;
            let (from, to) = parse_timing(block[timing]).ok_or(SubtitleError {
                line,
                message: "a timing line that cannot be read",
            })?;
            let text = block[timing + 1..]
                .iter()
                .map(|line| strip_markup(line))
                .collect::<Vec<_>>()
                .join("\n");
            if to > from && !text.trim().is_empty() {
                cues.push(Cue {
                    start: from,
                    end: to,
                    text: text.into(),
                });
            }
        }
        cues.sort_by_key(|cue| cue.start);
        Ok(Self { cues: cues.into() })
    }

    /// Every cue, by start time.
    pub fn cues(&self) -> &[Cue] {
        &self.cues
    }

    /// The cues showing at `position`, by start time (several can overlap).
    pub fn at(&self, position: Duration) -> impl Iterator<Item = &Cue> {
        // Cues starting after `position` cannot show yet.
        let upto = self.cues.partition_point(|cue| cue.start <= position);
        self.cues[..upto]
            .iter()
            .filter(move |cue| position < cue.end)
    }

    /// The text showing at `position`, overlapping cues joined by line
    /// breaks; `None` when nothing shows.
    pub fn text_at(&self, position: Duration) -> Option<SharedString> {
        let mut showing = self.at(position).peekable();
        let first = showing.next()?;
        if showing.peek().is_none() {
            return Some(first.text.clone());
        }
        let mut text = first.text.to_string();
        for cue in showing {
            text.push('\n');
            text.push_str(&cue.text);
        }
        Some(text.into())
    }
}

fn is_webvtt_metadata(first_line: &str) -> bool {
    ["NOTE", "STYLE", "REGION"].iter().any(|keyword| {
        first_line == *keyword
            || first_line
                .strip_prefix(keyword)
                .is_some_and(|rest| rest.starts_with([' ', '\t']))
    })
}

/// `00:01:02.500 --> 00:01:04.000 align:start` (WebVTT) or
/// `00:01:02,500 --> 00:01:04,000` (SubRip).
fn parse_timing(line: &str) -> Option<(Duration, Duration)> {
    let (from, rest) = line.split_once("-->")?;
    let to = rest.split_whitespace().next()?;
    Some((parse_time(from.trim())?, parse_time(to)?))
}

/// `hh:mm:ss.mmm`, `mm:ss.mmm`, with `.` or `,` before the milliseconds.
fn parse_time(text: &str) -> Option<Duration> {
    let (clock, millis) = text.split_once(['.', ','])?;
    if millis.len() != 3 || !millis.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let millis: u64 = millis.parse().ok()?;
    let parts: Vec<&str> = clock.split(':').collect();
    let number = |part: &str| -> Option<u64> {
        (!part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| part.parse().ok())
            .flatten()
    };
    let (hours, minutes, seconds) = match parts.as_slice() {
        [hours, minutes, seconds] => (number(hours)?, number(minutes)?, number(seconds)?),
        [minutes, seconds] => (0, number(minutes)?, number(seconds)?),
        _ => return None,
    };
    if minutes > 59 || seconds > 59 {
        return None;
    }
    let total = (hours * 3600 + minutes * 60 + seconds) * 1000 + millis;
    Some(Duration::from_millis(total))
}

/// Drops `<i>`, `<c.name>`, `<v Speaker>`, `<00:00:01.000>` and SubRip's
/// `{\an8}`, and decodes the entities WebVTT allows.
fn strip_markup(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(char) = chars.next() {
        match char {
            '<' => {
                for next in chars.by_ref() {
                    if next == '>' {
                        break;
                    }
                }
            }
            '{' if chars.peek() == Some(&'\\') => {
                for next in chars.by_ref() {
                    if next == '}' {
                        break;
                    }
                }
            }
            _ => out.push(char),
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", "\u{a0}")
        .replace("&lrm;", "\u{200e}")
        .replace("&rlm;", "\u{200f}")
        .replace("&amp;", "&")
}

/// Shows the subtitles for `player`: the cue due at its position from a
/// sidecar file given with [`VideoPlayer::set_subtitles`] (or
/// [`Captions::subtitles`]), or else the text of the text track chosen
/// inside the media
/// ([`VideoPlayer::select_text_track`]). Put it over the
/// [`video()`](crate::video) element, in a parent with `relative()`: by
/// default it sits at the bottom centre, as white text on a dark band, and
/// takes the usual style methods to change that.
///
/// While the picture plays, the window draws every frame, so the text
/// follows the position. With a backend whose picture shows by itself (the
/// web's), GPUI cannot draw over it at all.
///
/// ```no_run
/// # use gpui::{Entity, IntoElement, ParentElement, Styled, div};
/// # use gpui_av::{VideoPlayer, captions, video};
/// # fn example(player: &Entity<VideoPlayer>) -> impl IntoElement {
/// div()
///     .relative()
///     .size_full()
///     .child(video(player).size_full())
///     .child(captions(player).bottom_8())
/// # }
/// ```
pub fn captions(player: &Entity<VideoPlayer>) -> Captions {
    Captions {
        player: player.clone(),
        subtitles: None,
        base: div(),
    }
}

/// The element [`captions()`] returns.
#[derive(IntoElement)]
pub struct Captions {
    player: Entity<VideoPlayer>,
    subtitles: Option<Subtitles>,
    base: Div,
}

impl Captions {
    /// Shows these sidecar subtitles instead of the media's own text.
    pub fn subtitles(mut self, subtitles: &Subtitles) -> Self {
        self.subtitles = Some(subtitles.clone());
        self
    }
}

impl Styled for Captions {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl RenderOnce for Captions {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let player = self.player.read(cx);
        let own = player
            .subtitles()
            .filter(|_| !player.session_draws_subtitles());
        let text = match self.subtitles.as_ref().or(own) {
            Some(subtitles) => subtitles.text_at(player.position()),
            // The session draws the sidecar subtitles itself.
            None if player.subtitles().is_some() => None,
            None => player.text(),
        };
        let band = text.map(|text| {
            div()
                .px(px(10.))
                .py(px(4.))
                .rounded(px(4.))
                .bg(rgba(0x000000b8))
                .text_color(rgba(0xffffffff))
                .flex()
                .flex_col()
                .items_center()
                .children(
                    text.split('\n')
                        .map(|line| div().child(SharedString::from(line.to_string())))
                        .collect::<Vec<_>>(),
                )
        });
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom(px(24.))
            .flex()
            .justify_center()
            .child(self.base.children(band))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    #[test]
    fn webvtt_cues_with_settings_notes_and_markup() {
        let text = "\u{feff}WEBVTT - a film\nKind: captions\n\nNOTE written by hand\nover two lines\n\nSTYLE\n::cue { color: red }\n\nintro\n00:01.000 --> 00:02.500 align:start line:0\n<v Ana>Hello <i>there</i></v>\n&lt;sigh&gt; &amp; more\n\n01:00:00.000 --> 01:00:01.000\nAn hour in\n";
        let subtitles = Subtitles::parse(text).unwrap();
        assert_eq!(
            subtitles.cues(),
            [
                Cue {
                    start: ms(1000),
                    end: ms(2500),
                    text: "Hello there\n<sigh> & more".into(),
                },
                Cue {
                    start: ms(3_600_000),
                    end: ms(3_601_000),
                    text: "An hour in".into(),
                },
            ]
        );
    }

    #[test]
    fn subrip_cues_with_numbers_and_tags() {
        let text = "1\r\n00:00:01,000 --> 00:00:02,000\r\n{\\an8}<i>Top</i>\r\n\r\n2\r\n00:00:03,250 --> 00:00:04,000\r\nTwo\r\nlines\r\n";
        let subtitles = Subtitles::parse(text).unwrap();
        assert_eq!(subtitles.cues().len(), 2);
        assert_eq!(subtitles.cues()[0].text, "Top");
        assert_eq!(subtitles.cues()[1].start, ms(3250));
        assert_eq!(subtitles.cues()[1].text, "Two\nlines");
    }

    #[test]
    fn the_text_at_a_position_joins_overlapping_cues() {
        let text = "WEBVTT\n\n00:01.000 --> 00:05.000\nlong\n\n00:02.000 --> 00:03.000\nshort\n";
        let subtitles = Subtitles::parse(text).unwrap();
        assert_eq!(subtitles.text_at(ms(500)), None);
        assert_eq!(subtitles.text_at(ms(1000)).as_deref(), Some("long"));
        assert_eq!(subtitles.text_at(ms(2500)).as_deref(), Some("long\nshort"));
        // The end is not included.
        assert_eq!(subtitles.text_at(ms(3000)).as_deref(), Some("long"));
        assert_eq!(subtitles.text_at(ms(5000)), None);
    }

    #[test]
    fn a_broken_timing_line_names_its_line() {
        let error = Subtitles::parse("WEBVTT\n\n00:01.000 --> soon\ntext\n").unwrap_err();
        assert_eq!(error.line(), 3);
        let error = Subtitles::parse("1\n00:00:01,000 -> 00:00:02,000\ntext\n").unwrap_err();
        assert_eq!(error.line(), 1);
    }

    #[test]
    fn times_take_both_separators_and_reject_nonsense() {
        assert_eq!(parse_time("00:00:01,500"), Some(ms(1500)));
        assert_eq!(parse_time("02:03.004"), Some(ms(123_004)));
        assert_eq!(parse_time("1:2:3.000"), Some(ms(3_723_000)));
        assert_eq!(parse_time("00:61.000"), None);
        assert_eq!(parse_time("00:01.5"), None);
        assert_eq!(parse_time("aa:01.500"), None);
    }
}
