//! The audio and text tracks inside the media, where the browser lists
//! them: Safari lists both; Chrome and Firefox list neither for media they
//! play themselves, so there the list stays empty.
//!
//! The track lists are read by name rather than through typed bindings,
//! since `audioTracks` is missing from most browsers.

use gpui_av::{Track, TrackKind};
use wasm_bindgen::JsValue;

/// The text track kinds that are subtitles: the rest (chapters, metadata)
/// are not for showing.
const SHOWN_KINDS: [&str; 2] = ["subtitles", "captions"];

/// Labels that are the name muxers give every track's handler, not a
/// title: Chrome offers an MP4 track's handler name as its label.
const HANDLER_NAMES: [&str; 4] = [
    "SoundHandler",
    "SubtitleHandler",
    "VideoHandler",
    "TextHandler",
];

fn get(value: &JsValue, name: &str) -> Option<JsValue> {
    js_sys::Reflect::get(value, &name.into())
        .ok()
        .filter(|value| !value.is_undefined() && !value.is_null())
}

fn get_string(value: &JsValue, name: &str) -> Option<String> {
    get(value, name)
        .and_then(|value| value.as_string())
        .filter(|text| !text.is_empty())
}

fn set(value: &JsValue, name: &str, to: &JsValue) {
    if js_sys::Reflect::set(value, &name.into(), to).is_err() {
        web_sys::console::warn_1(&format!("gpui-av: could not set a track's {name}").into());
    }
}

/// The items of a track list (`audioTracks`, `textTracks`, `activeCues`).
fn items(list: Option<JsValue>) -> Vec<JsValue> {
    let Some(list) = list else {
        return Vec::new();
    };
    let length = get(&list, "length")
        .and_then(|length| length.as_f64())
        .unwrap_or(0.0) as u32;
    (0..length)
        .filter_map(|index| js_sys::Reflect::get_u32(&list, index).ok())
        .filter(|item| !item.is_undefined())
        .collect()
}

fn audio_tracks(video: &JsValue) -> Vec<JsValue> {
    items(get(video, "audioTracks"))
}

fn text_tracks(video: &JsValue) -> Vec<JsValue> {
    items(get(video, "textTracks"))
        .into_iter()
        .filter(|track| {
            get_string(track, "kind").is_some_and(|kind| SHOWN_KINDS.contains(&kind.as_str()))
        })
        .collect()
}

fn describe(kind: TrackKind, index: usize, track: &JsValue) -> Track {
    let mut entry = Track::new(kind, index);
    if let Some(language) = get_string(track, "language") {
        entry = entry.with_language(language);
    }
    if let Some(label) =
        get_string(track, "label").filter(|label| !HANDLER_NAMES.contains(&label.as_str()))
    {
        entry = entry.with_title(label);
    }
    entry
}

/// The tracks, and the audio track playing.
pub(crate) fn read(video: &JsValue) -> (Vec<Track>, Option<usize>) {
    let audio = audio_tracks(video);
    let audio_track = audio
        .iter()
        .position(|track| get(track, "enabled").and_then(|on| on.as_bool()) == Some(true));
    let tracks = audio
        .iter()
        .enumerate()
        .map(|(index, track)| describe(TrackKind::Audio, index, track))
        .chain(
            text_tracks(video)
                .iter()
                .enumerate()
                .map(|(index, track)| describe(TrackKind::Text, index, track)),
        )
        .collect();
    (tracks, audio_track)
}

/// Plays the audio track at `index` and no other.
pub(crate) fn select_audio(video: &JsValue, index: usize) {
    let audio = audio_tracks(video);
    if index >= audio.len() {
        return;
    }
    for (position, track) in audio.iter().enumerate() {
        set(track, "enabled", &JsValue::from_bool(position == index));
    }
}

/// Keeps the text track at `index` on, and every other one off. The one
/// on is `showing`: the browser draws it over the picture, since GPUI
/// cannot draw over the video element; its cues are still read for
/// [`text`]. A browser may turn a default track on by itself, so this runs
/// again whenever the tracks are read. Returns the track on, if it exists.
pub(crate) fn apply_text(video: &JsValue, index: Option<usize>) -> Option<usize> {
    let text = text_tracks(video);
    for (position, track) in text.iter().enumerate() {
        let mode = if Some(position) == index {
            "showing"
        } else {
            "disabled"
        };
        if get_string(track, "mode").as_deref() != Some(mode) {
            set(track, "mode", &JsValue::from_str(mode));
        }
    }
    index.filter(|index| *index < text.len())
}

/// The text of the cues showing on the text track at `index`, markup
/// removed and lines separated by `\n`.
pub(crate) fn text(video: &JsValue, index: usize) -> Option<String> {
    let track = text_tracks(video).into_iter().nth(index)?;
    let lines: Vec<String> = items(get(&track, "activeCues"))
        .iter()
        .filter_map(|cue| get_string(cue, "text"))
        .map(|text| plain(&text))
        .filter(|text| !text.is_empty())
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// WebVTT cue text without its tags (`<i>`, `<c.yellow>`, `<00:01.000>`)
/// and with its character references decoded.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for character in text.chars() {
        match character {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(character),
            _ => {}
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", "\u{a0}")
        .replace("&lrm;", "\u{200e}")
        .replace("&rlm;", "\u{200f}")
        .replace("&amp;", "&")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::plain;

    #[test]
    fn cue_text_loses_its_markup() {
        assert_eq!(
            plain("<i>Hello</i> <c.yellow>there</c> &amp; <00:01.000>you &lt;3"),
            "Hello there & you <3"
        );
    }
}
