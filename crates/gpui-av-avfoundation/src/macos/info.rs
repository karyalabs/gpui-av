//! Reading [`MediaInfo`] from a loaded `AVPlayerItem`.

use std::time::Duration;

use gpui::size;
use gpui_av::{ColorInfo, ColorMatrix, ColorRange, MediaInfo, Rotation};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_av_foundation::{AVAssetTrack, AVMediaTypeAudio, AVMediaTypeVideo, AVPlayerItem};
use objc2_core_foundation::{CFBoolean, CFRetained, CFString, CGAffineTransform};
use objc2_core_media::{
    CMFormatDescription, CMTime, CMTimeFlags, kCMFormatDescriptionExtension_FullRangeVideo,
    kCMFormatDescriptionExtension_YCbCrMatrix, kCMFormatDescriptionYCbCrMatrix_ITU_R_601_4,
    kCMFormatDescriptionYCbCrMatrix_ITU_R_709_2, kCMFormatDescriptionYCbCrMatrix_ITU_R_2020,
};

/// What the session needs to know about the media beyond [`MediaInfo`].
pub(crate) struct Loaded {
    pub info: MediaInfo,
    /// The shortest time between two video frames, for counting drops
    /// where the track has no sample cursors.
    pub frame_duration: Option<Duration>,
    /// The video track, when it can list its samples (local files can),
    /// so drops are counted against the frames that really exist.
    pub sample_track: Option<Retained<AVAssetTrack>>,
    /// A track this Mac has no decoder for, by its codec's four-character
    /// code. AVPlayer plays such a track as nothing instead of failing.
    pub undecodable: Option<String>,
}

/// A finite, non-negative `CMTime` as a `Duration`.
pub(crate) fn duration(time: CMTime) -> Option<Duration> {
    let flags = time.flags;
    if !flags.contains(CMTimeFlags::Valid)
        || flags.intersects(
            CMTimeFlags::Indefinite | CMTimeFlags::PositiveInfinity | CMTimeFlags::NegativeInfinity,
        )
        || time.timescale <= 0
    {
        return None;
    }
    let seconds = time.value as f64 / f64::from(time.timescale);
    (seconds.is_finite() && seconds >= 0.0).then(|| Duration::from_secs_f64(seconds))
}

/// Reads the media's properties. Call once the item is ready to play, when
/// AVFoundation has loaded the tracks, so nothing here blocks.
pub(crate) fn read(item: &AVPlayerItem) -> Loaded {
    // SAFETY: plain property reads on a live item and its asset.
    let (item_duration, tracks) = unsafe { (item.duration(), item.asset().tracks()) };
    let mut info = MediaInfo {
        duration: duration(item_duration),
        ..MediaInfo::default()
    };
    let mut has_audio = false;
    let mut frame_duration = None;
    let mut sample_track = None;
    let mut undecodable = None;
    // SAFETY: constant NSStrings exported by AVFoundation.
    let (video, audio) = unsafe { (AVMediaTypeVideo, AVMediaTypeAudio) };
    for track in tracks.iter() {
        // SAFETY: property reads on a loaded track.
        let (media_type, enabled) = unsafe { (track.mediaType(), track.isEnabled()) };
        if !enabled {
            continue;
        }
        // SAFETY: a property read on a loaded track.
        if undecodable.is_none() && !unsafe { track.isDecodable() } {
            undecodable = Some(codec_name(&track));
        }
        if audio.is_some_and(|audio| *media_type == *audio) {
            has_audio = true;
        } else if video.is_some_and(|video| *media_type == *video) && !info.has_video {
            info.has_video = true;
            read_video(&track, &mut info);
            // SAFETY: a property read on a loaded track.
            frame_duration =
                duration(unsafe { track.minFrameDuration() }).filter(|frame| !frame.is_zero());
            // SAFETY: a property read on a loaded track.
            if unsafe { track.canProvideSampleCursors() } {
                sample_track = Some(track.clone());
            }
        }
    }
    info.has_audio = Some(has_audio);
    Loaded {
        info,
        frame_duration,
        sample_track,
        undecodable,
    }
}

/// The track's codec as its four-character code, such as `avc1`.
fn codec_name(track: &AVAssetTrack) -> String {
    // SAFETY: a property read on a loaded track; the array holds
    // CMFormatDescriptions.
    let code = unsafe {
        track.formatDescriptions().firstObject().map(|description| {
            let description: &CMFormatDescription =
                &*(&*description as *const AnyObject).cast::<CMFormatDescription>();
            description.media_sub_type()
        })
    };
    match code {
        Some(code) => String::from_utf8_lossy(&code.to_be_bytes()).into_owned(),
        None => "unknown".to_string(),
    }
}

fn read_video(track: &AVAssetTrack, info: &mut MediaInfo) {
    // SAFETY: property reads on a loaded track.
    let (natural, transform) = unsafe { (track.naturalSize(), track.preferredTransform()) };
    let rotation = rotation(transform);
    info.rotation = Some(rotation);
    let (width, height) = (natural.width.round().abs(), natural.height.round().abs());
    if width > 0.0 && height > 0.0 {
        let (width, height) = (width as u32, height as u32);
        info.natural_size = Some(match rotation {
            Rotation::Cw90 | Rotation::Cw270 => size(height, width),
            _ => size(width, height),
        });
    }
    // SAFETY: a property read on a loaded track.
    let descriptions = unsafe { track.formatDescriptions() };
    if let Some(first) = descriptions.firstObject() {
        info.color = color(&first);
    }
}

/// The rotation in a track's transform. Transforms that are not quarter
/// turns (mirroring, shear) report no rotation.
fn rotation(transform: CGAffineTransform) -> Rotation {
    let degrees = transform.b.atan2(transform.a).to_degrees();
    let quarter = ((degrees / 90.0).round() as i64).rem_euclid(4);
    match quarter {
        1 => Rotation::Cw90,
        2 => Rotation::Cw180,
        3 => Rotation::Cw270,
        _ => Rotation::None,
    }
}

/// The colour range and matrix the format description states. A field the
/// container leaves out stays `None`.
fn color(description: &AnyObject) -> ColorInfo {
    // SAFETY: AVAssetTrack.formatDescriptions holds CMFormatDescriptions,
    // which are toll-free bridged CF objects.
    let description: &CMFormatDescription =
        unsafe { &*(description as *const AnyObject).cast::<CMFormatDescription>() };
    // SAFETY: constant CFString keys, read from a live format description.
    let (range, matrix) = unsafe {
        (
            description.extension(kCMFormatDescriptionExtension_FullRangeVideo),
            description.extension(kCMFormatDescriptionExtension_YCbCrMatrix),
        )
    };
    let range = range
        .and_then(|value| value.downcast::<CFBoolean>().ok())
        .map(|full| {
            if full.as_bool() {
                ColorRange::Full
            } else {
                ColorRange::Limited
            }
        });
    let matrix = matrix
        .and_then(|value| value.downcast::<CFString>().ok())
        .and_then(|name| matrix_named(&name));
    ColorInfo { range, matrix }
}

fn matrix_named(name: &CFRetained<CFString>) -> Option<ColorMatrix> {
    // SAFETY: constant CFStrings exported by CoreMedia.
    let known = unsafe {
        [
            (
                kCMFormatDescriptionYCbCrMatrix_ITU_R_709_2,
                ColorMatrix::Bt709,
            ),
            (
                kCMFormatDescriptionYCbCrMatrix_ITU_R_601_4,
                ColorMatrix::Bt601,
            ),
            (
                kCMFormatDescriptionYCbCrMatrix_ITU_R_2020,
                ColorMatrix::Bt2020,
            ),
        ]
    };
    known
        .into_iter()
        .find(|(constant, _)| **name == **constant)
        .map(|(_, matrix)| matrix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(degrees: f64) -> CGAffineTransform {
        let radians = degrees.to_radians();
        CGAffineTransform {
            a: radians.cos(),
            b: radians.sin(),
            c: -radians.sin(),
            d: radians.cos(),
            tx: 0.0,
            ty: 0.0,
        }
    }

    #[test]
    fn quarter_turns_map_to_rotations() {
        assert_eq!(rotation(turn(0.0)), Rotation::None);
        assert_eq!(rotation(turn(90.0)), Rotation::Cw90);
        assert_eq!(rotation(turn(180.0)), Rotation::Cw180);
        assert_eq!(rotation(turn(-90.0)), Rotation::Cw270);
    }

    #[test]
    fn invalid_times_have_no_duration() {
        let time = CMTime {
            value: 0,
            timescale: 0,
            flags: CMTimeFlags::empty(),
            epoch: 0,
        };
        assert_eq!(duration(time), None);
        let time = CMTime {
            value: 3,
            timescale: 2,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        };
        assert_eq!(duration(time), Some(Duration::from_millis(1500)));
    }
}
