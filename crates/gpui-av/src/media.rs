use std::time::Duration;

use gpui::Size;

/// What a backend learned about the media. Every field is what the decoder
/// or container reported; a backend leaves a field empty instead of guessing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaInfo {
    /// The length, when the media has one.
    pub duration: Option<Duration>,
    /// The size of the picture as shown, in pixels: after the rotation tag
    /// and the pixel aspect ratio are applied. `None` when there is no video.
    pub natural_size: Option<Size<u32>>,
    /// The rotation tag of the video track; `None` when there is no video
    /// or the backend cannot tell (a browser applies the tag without
    /// saying what it was). Either way the backend has already applied it
    /// to the frames it draws and to `natural_size`.
    pub rotation: Option<Rotation>,
    /// Whether there is a video track.
    pub has_video: bool,
    /// Whether there is an audio track; `None` when the backend cannot
    /// tell (some browsers do not say).
    pub has_audio: Option<bool>,
    /// How the video encodes colour.
    pub color: ColorInfo,
    /// The audio and text tracks inside the media, in the order the
    /// container lists each kind. Empty when the backend cannot list them.
    pub tracks: Vec<Track>,
    /// The audio track playing, as an index into the audio tracks.
    /// `None` when there is no audio or the backend cannot tell.
    pub audio_track: Option<usize>,
    /// The text track shown, as an index into the text tracks; `None`
    /// when subtitles inside the media are off (the default) or there are
    /// none.
    pub text_track: Option<usize>,
}

/// What kind of track a [`Track`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TrackKind {
    /// Sound: a language, a commentary, a description.
    Audio,
    /// Subtitles or captions carried inside the media.
    Text,
}

/// An audio or text track inside the media, as the container labels it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Track {
    /// Audio or text.
    pub kind: TrackKind,
    /// Its place among the tracks of its kind, from 0: what
    /// [`VideoPlayer::select_audio_track`](crate::VideoPlayer::select_audio_track)
    /// and
    /// [`VideoPlayer::select_text_track`](crate::VideoPlayer::select_text_track)
    /// take.
    pub index: usize,
    /// The language as the media states it (usually an ISO 639 code such
    /// as `en` or `fra`), when it does.
    pub language: Option<String>,
    /// The track's title, when the media gives one.
    pub title: Option<String>,
}

impl Track {
    /// A track with no language or title.
    pub fn new(kind: TrackKind, index: usize) -> Self {
        Self {
            kind,
            index,
            language: None,
            title: None,
        }
    }

    /// The same track with this language.
    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }

    /// The same track with this title.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }
}

/// A clockwise rotation in quarter turns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Rotation {
    /// Not rotated.
    #[default]
    None,
    /// 90 degrees clockwise.
    Cw90,
    /// 180 degrees.
    Cw180,
    /// 270 degrees clockwise.
    Cw270,
}

impl Rotation {
    /// The rotation in degrees clockwise.
    pub fn degrees(self) -> u32 {
        match self {
            Rotation::None => 0,
            Rotation::Cw90 => 90,
            Rotation::Cw180 => 180,
            Rotation::Cw270 => 270,
        }
    }
}

/// The colour properties the decoder reported. `None` means the backend
/// cannot tell (a browser's `<video>` does not say).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColorInfo {
    /// Limited or full range.
    pub range: Option<ColorRange>,
    /// The YUV to RGB matrix.
    pub matrix: Option<ColorMatrix>,
    /// How the media maps code values to brightness: standard range or one
    /// of the HDR curves. This describes the media, not the picture on
    /// screen: a backend may draw a standard range rendition of HDR media
    /// (the macOS backend does, as GPUI draws in standard range).
    pub transfer: Option<TransferFunction>,
}

/// Whether brightness runs 16 to 235 or 0 to 255 (for 8-bit video).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColorRange {
    /// 16 to 235, which almost all video uses.
    Limited,
    /// 0 to 255.
    Full,
}

/// The YUV to RGB matrix of the video.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ColorMatrix {
    /// ITU-R BT.601 (standard definition).
    Bt601,
    /// ITU-R BT.709 (high definition).
    Bt709,
    /// ITU-R BT.2020 (ultra high definition, HDR).
    Bt2020,
}

/// How the video maps code values to brightness.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TransferFunction {
    /// A standard dynamic range curve (BT.709, BT.601, sRGB and the like).
    Sdr,
    /// The perceptual quantizer, SMPTE ST 2084: HDR10 and Dolby Vision.
    Pq,
    /// Hybrid log-gamma, ARIB STD-B67: broadcast and phone HDR.
    Hlg,
}

impl TransferFunction {
    /// Whether this is a high dynamic range curve.
    pub fn is_hdr(self) -> bool {
        matches!(self, TransferFunction::Pq | TransferFunction::Hlg)
    }
}
