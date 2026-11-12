//! The corpus manifest: what each clip is and what a backend must report
//! for it (`corpus/tier-a.json`).

use std::time::Duration;

use gpui_av::{ColorMatrix, ColorRange, MediaInfo};
use serde::{Deserialize, Serialize};

/// A corpus: a list of clips with their expected properties.
#[derive(Clone, Debug, Deserialize)]
pub struct Manifest {
    /// The clips, in the order they are benched.
    pub clips: Vec<Expected>,
}

impl Manifest {
    /// Reads a manifest from its JSON text.
    pub fn parse(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|error| format!("not a corpus manifest: {error}"))
    }
}

/// What one clip is expected to be. Fields that do not apply are left out.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Expected {
    /// The file name, relative to the clips folder or the base URL.
    pub file: String,
    /// Whether the clip must play or must fail.
    pub expect: Outcome,
    /// For a clip that must fail: the error kinds that are right, by name
    /// (`"Decode"`, `"NotFound"` and so on).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    /// The picture's width as shown, after rotation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    /// The picture's height as shown, after rotation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// The rotation tag, in degrees clockwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<u32>,
    /// The frame rate; `None` for variable frame rate or no video.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<f64>,
    /// The length in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    /// The video codec; `None` when there is no video track.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_codec: Option<String>,
    /// The audio codec; `None` when there is no audio track.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_codec: Option<String>,
    /// `"limited"` or `"full"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_range: Option<String>,
    /// `"bt601"`, `"bt709"` or `"bt2020"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_matrix: Option<String>,
    /// How long the bench plays this clip, when not the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub play_seconds: Option<f64>,
}

/// Whether a clip must play or must fail.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// The clip plays.
    #[default]
    Plays,
    /// The clip ends in `Failed`.
    Fails,
}

/// How what a backend reported compares with the manifest. `Some(true)` is
/// a match, `Some(false)` a mismatch, and `None` means there was nothing to
/// compare: the manifest does not say, or the backend reported the value
/// as unknown (as a browser does for colour).
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
pub struct Checks {
    /// The picture's size as shown.
    pub size: Option<bool>,
    /// The rotation tag.
    pub rotation: Option<bool>,
    /// The length, within [`DURATION_TOLERANCE`].
    pub duration: Option<bool>,
    /// Which of video and audio the media has.
    pub tracks: Option<bool>,
    /// The colour range.
    pub color_range: Option<bool>,
    /// The colour matrix.
    pub color_matrix: Option<bool>,
}

/// How far a reported length may be from the manifest's.
pub const DURATION_TOLERANCE: Duration = Duration::from_millis(150);

impl Expected {
    /// Compares what a backend reported with this entry.
    pub fn check(&self, info: &MediaInfo) -> Checks {
        let has_video = self.video_codec.is_some();
        let has_audio = self.audio_codec.is_some();
        let size = match (self.width, self.height) {
            (Some(width), Some(height)) => Some(
                info.natural_size
                    .is_some_and(|size| size.width == width && size.height == height),
            ),
            // No picture expected: there must be no size either.
            _ if !has_video => Some(info.natural_size.is_none()),
            _ => None,
        };
        let duration = self.duration.map(|expected| {
            info.duration.is_some_and(|reported| {
                reported.abs_diff(Duration::from_secs_f64(expected)) <= DURATION_TOLERANCE
            })
        });
        let range =
            self.color_range
                .as_deref()
                .zip(info.color.range)
                .map(|(expected, reported)| {
                    expected
                        == match reported {
                            ColorRange::Limited => "limited",
                            ColorRange::Full => "full",
                        }
                });
        let matrix =
            self.color_matrix
                .as_deref()
                .zip(info.color.matrix)
                .map(|(expected, reported)| {
                    expected
                        == match reported {
                            ColorMatrix::Bt601 => "bt601",
                            ColorMatrix::Bt709 => "bt709",
                            ColorMatrix::Bt2020 => "bt2020",
                            _ => "other",
                        }
                });
        Checks {
            size,
            rotation: self
                .rotation
                .map(|expected| info.rotation.degrees() == expected),
            duration,
            tracks: Some(info.has_video == has_video && info.has_audio == has_audio),
            color_range: range,
            color_matrix: matrix,
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::size;
    use gpui_av::{ColorInfo, Rotation};

    use super::*;

    fn tier_a() -> Manifest {
        Manifest::parse(include_str!("../../corpus/tier-a.json")).unwrap()
    }

    fn entry(file: &str) -> Expected {
        tier_a()
            .clips
            .into_iter()
            .find(|clip| clip.file == file)
            .unwrap()
    }

    #[test]
    fn the_tier_a_manifest_parses_and_names_each_clip_once() {
        let manifest = tier_a();
        let mut files: Vec<_> = manifest.clips.iter().map(|clip| &clip.file).collect();
        assert!(files.len() >= 15);
        files.sort();
        files.dedup();
        assert_eq!(files.len(), manifest.clips.len());
        for clip in &manifest.clips {
            assert_eq!(
                clip.expect == Outcome::Fails,
                !clip.errors.is_empty(),
                "{}: a clip lists error kinds exactly when it must fail",
                clip.file
            );
        }
    }

    #[test]
    fn a_matching_report_passes_every_check() {
        let info = MediaInfo {
            duration: Some(Duration::from_millis(10_033)),
            natural_size: Some(size(720, 1280)),
            rotation: Rotation::Cw90,
            has_video: true,
            has_audio: true,
            color: ColorInfo {
                range: Some(ColorRange::Limited),
                matrix: Some(ColorMatrix::Bt709),
            },
        };
        let checks = entry("h264-1280x720-rotate90.mp4").check(&info);
        assert_eq!(
            checks,
            Checks {
                size: Some(true),
                rotation: Some(true),
                duration: Some(true),
                tracks: Some(true),
                color_range: Some(true),
                color_matrix: Some(true),
            }
        );
    }

    #[test]
    fn mismatches_and_unknowns_are_told_apart() {
        // A backend that did not apply the rotation and cannot see colour.
        let info = MediaInfo {
            duration: Some(Duration::from_secs(12)),
            natural_size: Some(size(1280, 720)),
            has_video: true,
            ..MediaInfo::default()
        };
        let checks = entry("h264-1280x720-rotate90.mp4").check(&info);
        assert_eq!(
            checks,
            Checks {
                size: Some(false),
                rotation: Some(false),
                duration: Some(false),
                tracks: Some(false),
                color_range: None,
                color_matrix: None,
            }
        );
    }

    #[test]
    fn audio_only_expects_no_picture() {
        let info = MediaInfo {
            duration: Some(Duration::from_secs(10)),
            has_audio: true,
            ..MediaInfo::default()
        };
        let checks = entry("aac-audio-only.m4a").check(&info);
        assert_eq!(checks.size, Some(true));
        assert_eq!(checks.tracks, Some(true));
    }
}
