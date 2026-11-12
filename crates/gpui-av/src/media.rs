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
    /// The rotation tag of the video track. The backend has already applied
    /// it to the frames it draws and to `natural_size`.
    pub rotation: Rotation,
    /// Whether there is a video track.
    pub has_video: bool,
    /// Whether there is an audio track.
    pub has_audio: bool,
    /// How the video encodes colour.
    pub color: ColorInfo,
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
