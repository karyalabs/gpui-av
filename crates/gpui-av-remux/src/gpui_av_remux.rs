//! Remuxing Matroska and WebM into fragmented MP4, for platforms whose
//! player decodes the codecs but will not open the container: AVFoundation
//! (through an HLS playlist of the segments) and browsers (through Media
//! Source Extensions). Spec section 5.3, step S7, experiment E7.
//!
//! Remuxing moves the coded frames from one container to another; nothing
//! is decoded or encoded, so it costs little and the platform's decoders
//! (hardware where there are some) still do the work. Pure Rust, no
//! `unsafe`, and it builds for the web.
//!
//! ```no_run
//! # fn main() -> Result<(), gpui_av_remux::RemuxError> {
//! let file = std::fs::File::open("film.mkv")?;
//! let mut remuxer = gpui_av_remux::Remuxer::open(std::io::BufReader::new(file))?;
//! let init = remuxer.init_segment();
//! for index in 0..remuxer.segments().len() {
//!     let segment = remuxer.media_segment(index)?;
//!     // Hand `init` and then each segment to the player.
//! }
//! # Ok(())
//! # }
//! ```

mod fragments;
mod matroska;
mod mp4;
#[cfg(not(target_arch = "wasm32"))]
mod proxy;
mod ranges;
#[cfg(not(target_arch = "wasm32"))]
mod serve;
mod sparse;
mod text;

pub use fragments::fragmented_mp4_length;
#[cfg(not(target_arch = "wasm32"))]
pub use proxy::{Fetch, HlsProxy, HttpResponse, HttpStream, resolve_url};
pub use ranges::{Fetched, RangeReader, RangeSource};
#[cfg(not(target_arch = "wasm32"))]
pub use serve::{Failure, FailureKind, HlsServer, Summary};
pub use sparse::{SparseFile, SparseReader};
pub use text::{Mp4Colour, Mp4Cue, Mp4TextTrack, mp4_text_tracks, mp4_video_colour};

use std::fmt;
use std::io::{self, Read, Seek};
use std::time::Duration;

use matroska::{Block, Ebml, Header, Kind};
use mp4::{Output, Sample};

/// Segments are at least this long, so a playlist of a long film stays
/// short and each request carries enough to be worth it.
const MIN_SEGMENT_NS: i64 = 3_000_000_000;

/// Why a file cannot be remuxed.
#[derive(Debug)]
#[non_exhaustive]
pub enum RemuxError {
    /// The bytes are not Matroska or WebM.
    NotMatroska,
    /// The file is Matroska, but holds something MP4 cannot carry, or this
    /// remuxer does not yet: the message says what.
    Unsupported(String),
    /// The file is damaged.
    Invalid(String),
    /// Reading failed.
    Io(io::Error),
}

impl RemuxError {
    pub(crate) fn invalid(message: &str) -> Self {
        Self::Invalid(message.to_string())
    }
}

impl fmt::Display for RemuxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotMatroska => formatter.write_str("not a Matroska or WebM file"),
            Self::Unsupported(what) => write!(formatter, "cannot remux {what} into MP4 yet"),
            Self::Invalid(what) => write!(formatter, "the file is damaged: {what}"),
            Self::Io(error) => write!(formatter, "reading the file failed: {error}"),
        }
    }
}

impl std::error::Error for RemuxError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for RemuxError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Whether `start`, the first bytes of a file, are Matroska or WebM's: an
/// EBML header. Four bytes are enough.
pub fn is_matroska(start: &[u8]) -> bool {
    start.starts_with(&[0x1A, 0x45, 0xDF, 0xA3])
}

/// One media segment: where it starts and how long it plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    /// When the segment starts.
    pub start: Duration,
    /// How long it plays.
    pub duration: Duration,
}

/// Where a segment's clusters are, and its times in nanoseconds.
#[derive(Clone, Copy, Debug)]
struct Plan {
    start_ns: i64,
    end_ns: i64,
    position: u64,
    end_position: Option<u64>,
}

/// A Matroska or WebM file, read as fragmented MP4: one init segment and
/// media segments that each start on a keyframe.
pub struct Remuxer<R> {
    ebml: Ebml<R>,
    header: Header,
    outputs: Vec<Output>,
    plans: Vec<Plan>,
    duration_ns: i64,
    skipped: Vec<String>,
    /// Each output's codec delay, which Matroska says to take off its
    /// block times.
    codec_delays: Vec<i64>,
    /// How far frames are presented after they are decoded, at most.
    reorder_ns: i64,
    /// How much later every output time is than the file's, so that no
    /// decode time is negative.
    shift_ns: i64,
}

impl<R: Read + Seek> Remuxer<R> {
    /// Reads the file's header and plans its segments. A file with cues
    /// (as muxers write by default) reads only its header here; one
    /// without is scanned, cluster headers only, once.
    ///
    /// The first video track and one audio track (the default one) are
    /// taken; the others are listed by [`Remuxer::skipped`]. A track whose
    /// codec MP4 cannot carry fails the whole file as
    /// [`RemuxError::Unsupported`], rather than playing without it.
    pub fn open(reader: R) -> Result<Self, RemuxError> {
        let mut ebml = Ebml::new(reader)?;
        let header = Header::read(&mut ebml)?;
        let media = |kind| {
            header
                .tracks
                .iter()
                .filter(move |track| track.kind == kind && track.enabled)
        };
        let video = media(Kind::Video).next().cloned();
        let audio = media(Kind::Audio)
            .find(|track| track.default)
            .or_else(|| media(Kind::Audio).next())
            .cloned();
        let mut skipped: Vec<String> = header
            .tracks
            .iter()
            .filter(|track| {
                Some(track.number) != video.as_ref().map(|track| track.number)
                    && Some(track.number) != audio.as_ref().map(|track| track.number)
            })
            .map(|track| format!("track {} ({})", track.number, track.codec_id))
            .collect();
        skipped.sort();
        let lead = video
            .as_ref()
            .or(audio.as_ref())
            .ok_or_else(|| RemuxError::Unsupported("a file with neither video nor audio".into()))?;
        let lead_number = lead.number;

        let first = first_cluster(&mut ebml, &header)?;
        let blocks_of = |number| first.iter().filter(move |block| block.track == number);
        // VP9 says its profile only in its frames.
        let first_frame = video
            .as_ref()
            .and_then(|track| blocks_of(track.number).next())
            .map(|block| block.data.as_slice());
        let mut outputs = Vec::new();
        let mut codec_delays = Vec::new();
        for track in video.iter().chain(audio.iter()) {
            let id = outputs.len() as u32 + 1;
            outputs.push(Output::new(id, track, first_frame)?);
            codec_delays.push(i64::try_from(track.codec_delay).unwrap_or(0));
        }
        // B-frames come before the frames they are presented after, so a
        // frame can be decoded up to this long before its own time. Decode
        // times start this much early; the first cluster holds a whole
        // group of pictures, which shows how far the encoder reorders.
        let reorder_ns = video.as_ref().map_or(0, |track| {
            let presentation: Vec<i64> =
                blocks_of(track.number).map(|block| block.time_ns).collect();
            let mut sorted = presentation.clone();
            sorted.sort_unstable();
            sorted
                .iter()
                .zip(&presentation)
                .map(|(decode, presented)| decode - presented)
                .max()
                .unwrap_or(0)
                .max(0)
        });
        // AAC's priming, and Opus' pre-skip taken off, start before zero.
        let earliest_audio = audio.as_ref().map_or(0, |track| {
            let delay = i64::try_from(track.codec_delay).unwrap_or(0);
            blocks_of(track.number)
                .map(|block| block.time_ns - delay)
                .min()
                .unwrap_or(0)
        });
        let shift_ns = reorder_ns.max(-earliest_audio).max(0);

        let scale = header.timestamp_scale as i64;
        let stated_ns = header
            .duration
            .map(|duration| (duration * scale as f64) as i64)
            .filter(|duration| *duration > 0);
        // Keyframe positions: the lead track's cues, else a scan.
        let mut starts: Vec<(i64, u64)> = header
            .cues
            .iter()
            .filter(|cue| cue.track == lead_number)
            .map(|cue| (cue.time as i64 * scale, header.segment_start + cue.cluster))
            .collect();
        let mut scanned_end = None;
        if starts.is_empty() {
            let clusters = matroska::scan_clusters(&mut ebml, &header, lead_number)?;
            scanned_end = clusters.last().map(|cluster| cluster.time as i64 * scale);
            starts = clusters
                .iter()
                .filter(|cluster| cluster.starts_with_keyframe)
                .map(|cluster| (cluster.time as i64 * scale, cluster.position))
                .collect();
        }
        starts.dedup_by_key(|start| start.1);
        if starts.is_empty() {
            return Err(RemuxError::invalid("no keyframe to start a segment on"));
        }
        // The first segment starts at the first cluster, whatever the cues
        // say.
        starts[0].1 = starts[0].1.min(header.first_cluster);
        let duration_ns = stated_ns
            .or(scanned_end)
            .unwrap_or(starts.last().map_or(0, |start| start.0) + 1_000_000_000);

        let mut plans: Vec<Plan> = Vec::new();
        for (time, position) in starts {
            match plans.last_mut() {
                Some(last) if time - last.start_ns < MIN_SEGMENT_NS => {}
                _ => {
                    if let Some(last) = plans.last_mut() {
                        last.end_ns = time;
                        last.end_position = Some(position);
                    }
                    plans.push(Plan {
                        start_ns: time,
                        end_ns: duration_ns,
                        position,
                        end_position: None,
                    });
                }
            }
        }
        // A last segment too short to stand alone joins the one before.
        if plans.len() >= 2
            && let Some(last) = plans.last().copied()
            && last.end_ns - last.start_ns < MIN_SEGMENT_NS / 3
        {
            plans.pop();
            if let Some(previous) = plans.last_mut() {
                previous.end_ns = last.end_ns;
                previous.end_position = None;
            }
        }
        if let Some(first) = plans.first_mut() {
            first.start_ns = first.start_ns.min(0);
        }

        Ok(Self {
            ebml,
            header,
            outputs,
            plans,
            duration_ns,
            skipped,
            codec_delays,
            reorder_ns,
            shift_ns,
        })
    }

    /// The `ftyp` and `moov` every media segment needs first.
    pub fn init_segment(&self) -> Vec<u8> {
        mp4::init_segment(&self.outputs, u64::try_from(self.duration_ns).ok())
    }

    /// The media segments, in order.
    pub fn segments(&self) -> Vec<Segment> {
        self.plans
            .iter()
            .map(|plan| Segment {
                start: Duration::from_nanos(plan.start_ns.max(0) as u64),
                duration: Duration::from_nanos((plan.end_ns - plan.start_ns).max(0) as u64),
            })
            .collect()
    }

    /// The length the file states, or the scan found.
    pub fn duration(&self) -> Duration {
        Duration::from_nanos(self.duration_ns.max(0) as u64)
    }

    /// The coded size of the video, for an HLS `RESOLUTION`.
    pub fn video_size(&self) -> Option<(u32, u32)> {
        self.outputs
            .iter()
            .find(|output| output.kind == Kind::Video)
            .map(|output| (output.width, output.height))
    }

    /// How much later every time in the output is than in the file, so that
    /// none is negative: B-frames and audio priming start before zero. A
    /// player of the segments through Media Source Extensions sets its
    /// `timestampOffset` to minus this; HLS players start the timeline at
    /// the first frame and need nothing.
    pub fn presentation_offset(&self) -> Duration {
        Duration::from_nanos(self.shift_ns.max(0) as u64)
    }

    /// Whether the output has a sound track.
    pub fn has_audio(&self) -> bool {
        self.outputs.iter().any(|output| output.kind == Kind::Audio)
    }

    /// `"webm"` or `"matroska"`.
    pub fn doc_type(&self) -> &str {
        &self.header.doc_type
    }

    /// The RFC 6381 codecs of the output, as Media Source Extensions and
    /// HLS want them: `avc1.64001f,mp4a.40.2`.
    pub fn codecs(&self) -> String {
        self.outputs
            .iter()
            .map(|output| output.codec.as_str())
            .collect::<Vec<_>>()
            .join(",")
    }

    /// The MIME type for `MediaSource.isTypeSupported` and `addSourceBuffer`.
    pub fn mime_type(&self) -> String {
        let has_video = self.outputs.iter().any(|output| output.kind == Kind::Video);
        let kind = if has_video { "video" } else { "audio" };
        format!("{kind}/mp4; codecs=\"{}\"", self.codecs())
    }

    /// Tracks the output leaves out (more audio tracks, subtitles), named
    /// by number and codec.
    pub fn skipped(&self) -> &[String] {
        &self.skipped
    }

    /// An HLS media playlist (version 7, fMP4) of the whole file, with the
    /// init segment at `init_uri` and segment `n` at `segment_uri(n)`.
    pub fn hls_playlist(&self, init_uri: &str, segment_uri: impl Fn(usize) -> String) -> String {
        let segments = self.segments();
        let target = segments
            .iter()
            .map(|segment| segment.duration.as_secs_f64().ceil() as u64)
            .max()
            .unwrap_or(1)
            .max(1);
        let mut playlist = format!(
            "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:{target}\n\
             #EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-INDEPENDENT-SEGMENTS\n\
             #EXT-X-MAP:URI=\"{init_uri}\"\n"
        );
        for (index, segment) in segments.iter().enumerate() {
            playlist += &format!(
                "#EXTINF:{:.6},\n{}\n",
                segment.duration.as_secs_f64(),
                segment_uri(index)
            );
        }
        playlist += "#EXT-X-ENDLIST\n";
        playlist
    }

    /// Where segment `index`'s clusters are in the file: from the first
    /// byte to the last (exclusive), or to the end of the file for `None`.
    /// A reader over a [`SparseFile`] needs these bytes fetched before
    /// [`Remuxer::media_segment`].
    pub fn segment_bytes(&self, index: usize) -> Option<(u64, Option<u64>)> {
        let plan = self.plans.get(index)?;
        Some((plan.position, plan.end_position.or(self.header.segment_end)))
    }

    /// Media segment `index`: a `moof` and an `mdat` with every frame of
    /// its clusters.
    pub fn media_segment(&mut self, index: usize) -> Result<Vec<u8>, RemuxError> {
        let plan = *self
            .plans
            .get(index)
            .ok_or_else(|| RemuxError::invalid("no such segment"))?;
        // A read that failed before (a sparse file's gap) leaves the
        // reader's position unknown.
        self.ebml.resync()?;
        let mut blocks: Vec<Vec<Block>> = vec![Vec::new(); self.outputs.len()];
        let outputs = &self.outputs;
        let codec_delays = &self.codec_delays;
        let shift_ns = self.shift_ns;
        matroska::read_clusters(
            &mut self.ebml,
            &self.header,
            plan.position,
            plan.end_position,
            |mut block| {
                if let Some(at) = outputs
                    .iter()
                    .position(|output| output.source == block.track)
                {
                    block.time_ns += shift_ns - codec_delays.get(at).copied().unwrap_or(0);
                    blocks[at].push(block);
                }
            },
        )?;
        let reorder_ns = self.reorder_ns;
        let tracks: Vec<(&Output, Vec<Sample>)> = outputs
            .iter()
            .zip(blocks)
            .map(|(output, blocks)| {
                let samples = match output.kind {
                    Kind::Video => {
                        video_samples(output, blocks, plan.end_ns + shift_ns, reorder_ns)
                    }
                    _ => audio_samples(output, blocks),
                };
                (output, samples)
            })
            .collect();
        Ok(mp4::media_segment(index as u32 + 1, &tracks))
    }
}

/// The blocks of the first cluster: where codecs that describe
/// themselves only in their frames do, and where the reordering and the
/// audio priming show.
fn first_cluster<R: Read + Seek>(
    ebml: &mut Ebml<R>,
    header: &Header,
) -> Result<Vec<Block>, RemuxError> {
    let mut blocks = Vec::new();
    let position = header.first_cluster;
    matroska::read_clusters(ebml, header, position, Some(position + 1), |block| {
        blocks.push(block);
    })?;
    Ok(blocks)
}

/// Video samples in decode order. Matroska stores only presentation
/// times; the decode times are those sorted, moved `reorder_ns` earlier so
/// that no frame is decoded after it is shown, and the difference is each
/// frame's composition offset. `end_ns` is when the segment's last frame
/// stops showing.
fn video_samples(output: &Output, blocks: Vec<Block>, end_ns: i64, reorder_ns: i64) -> Vec<Sample> {
    let presentation: Vec<i64> = blocks
        .iter()
        .map(|block| output.ticks(block.time_ns))
        .collect();
    let mut times: Vec<i64> = blocks.iter().map(|block| block.time_ns).collect();
    times.sort_unstable();
    let mut decode: Vec<i64> = times
        .iter()
        .map(|time| output.ticks(time - reorder_ns))
        .collect();
    // A later group of pictures may reorder further than the first did:
    // pull its decode times in so none comes after its frame is shown,
    // keeping them in order.
    let mut bound = i64::MAX;
    for index in (0..decode.len()).rev() {
        bound = bound.min(presentation[index]);
        decode[index] = decode[index].min(bound);
        bound = decode[index];
    }
    let end = output.ticks(end_ns - reorder_ns);
    let fallback = output
        .default_duration
        .map(|duration| output.ticks(duration as i64))
        .unwrap_or(0);
    // A fragment states only its first decode time and each sample's
    // length, so offsets are taken from where the lengths put each frame.
    let mut at = decode.first().copied().unwrap_or(0);
    let mut samples = Vec::with_capacity(blocks.len());
    for (index, block) in blocks.into_iter().enumerate() {
        let next = decode.get(index + 1).copied().unwrap_or(end);
        let mut duration = next - decode[index];
        if duration <= 0 || index + 1 == decode.len() && fallback > 0 && duration > fallback * 4 {
            duration = if fallback > 0 {
                fallback
            } else {
                samples
                    .last()
                    .map_or(1, |sample: &Sample| i64::from(sample.duration))
            };
        }
        let duration = duration.clamp(1, i64::from(u32::MAX));
        samples.push(Sample {
            decode_time: at,
            duration: duration as u32,
            composition_offset: (presentation[index] - at) as i32,
            keyframe: block.keyframe,
            data: block.data,
        });
        at += duration;
    }
    samples
}

/// Audio samples: frames of a known length are laid end to end from the
/// segment's first timestamp, since Matroska's millisecond times would
/// otherwise make each frame's length jitter around its true one.
fn audio_samples(output: &Output, blocks: Vec<Block>) -> Vec<Sample> {
    let Some(first) = blocks.first() else {
        return Vec::new();
    };
    let start = output.ticks(first.time_ns);
    let times: Vec<i64> = blocks
        .iter()
        .map(|block| output.ticks(block.time_ns))
        .collect();
    let mut at = start;
    let mut samples: Vec<Sample> = Vec::with_capacity(blocks.len());
    for (index, block) in blocks.into_iter().enumerate() {
        let known = frame_ticks(&output.codec, &block.data);
        let duration = known.unwrap_or_else(|| match times.get(index + 1) {
            Some(next) if *next > times[index] => next - times[index],
            _ => output
                .default_duration
                .map(|duration| output.ticks(duration as i64))
                .or_else(|| samples.last().map(|sample| i64::from(sample.duration)))
                .unwrap_or(1),
        });
        // Re-anchor on the stated time when it has moved more than a
        // frame away: a gap in the audio is real.
        if (times[index] - at).abs() > duration {
            at = times[index];
        }
        samples.push(Sample {
            decode_time: at,
            duration: duration.clamp(1, i64::from(u32::MAX)) as u32,
            composition_offset: 0,
            keyframe: true,
            data: block.data,
        });
        at += duration;
    }
    samples
}

/// How many ticks one frame of audio lasts, where the codec fixes it.
fn frame_ticks(codec: &str, data: &[u8]) -> Option<i64> {
    if codec.starts_with("mp4a.") {
        return Some(1024);
    }
    if codec == "opus" {
        return opus_ticks(data);
    }
    None
}

/// An Opus packet's length at 48 kHz, from its TOC byte (RFC 6716, 3.1).
fn opus_ticks(packet: &[u8]) -> Option<i64> {
    let toc = *packet.first()?;
    let config = toc >> 3;
    let per_frame: i64 = match config {
        0..=11 => [480, 960, 1920, 2880][usize::from(config % 4)],
        12..=15 => [480, 960][usize::from(config % 2)],
        _ => [120, 240, 480, 960][usize::from(config % 4)],
    };
    let frames: i64 = match toc & 0x03 {
        0 => 1,
        1 | 2 => 2,
        _ => i64::from(*packet.get(1)? & 0x3F),
    };
    Some(per_frame * frames)
}

#[cfg(test)]
mod tests {
    use super::opus_ticks;

    #[test]
    fn opus_packets_say_their_length() {
        // Config 9 (SILK wideband, 20 ms), one frame.
        assert_eq!(opus_ticks(&[9 << 3]), Some(960));
        // Config 31 (CELT fullband, 20 ms), code 3 with 3 frames.
        assert_eq!(opus_ticks(&[(31 << 3) | 3, 3]), Some(2880));
        assert_eq!(opus_ticks(&[]), None);
    }
}
