//! Reading Matroska and WebM: the header, the tracks, the cues, and the
//! blocks of a cluster. Only what remuxing needs; media data is read only
//! for the clusters asked for.

use std::io::{self, Read, Seek, SeekFrom};

use crate::RemuxError;

mod id {
    pub const EBML: u32 = 0x1A45_DFA3;
    pub const DOC_TYPE: u32 = 0x4282;
    pub const SEGMENT: u32 = 0x1853_8067;
    pub const SEEK_HEAD: u32 = 0x114D_9B74;
    pub const SEEK: u32 = 0x4DBB;
    pub const SEEK_ID: u32 = 0x53AB;
    pub const SEEK_POSITION: u32 = 0x53AC;
    pub const INFO: u32 = 0x1549_A966;
    pub const TIMESTAMP_SCALE: u32 = 0x2A_D7B1;
    pub const DURATION: u32 = 0x4489;
    pub const TRACKS: u32 = 0x1654_AE6B;
    pub const TRACK_ENTRY: u32 = 0xAE;
    pub const TRACK_NUMBER: u32 = 0xD7;
    pub const TRACK_TYPE: u32 = 0x83;
    pub const FLAG_ENABLED: u32 = 0xB9;
    pub const FLAG_DEFAULT: u32 = 0x88;
    pub const DEFAULT_DURATION: u32 = 0x23_E383;
    pub const NAME: u32 = 0x536E;
    pub const LANGUAGE: u32 = 0x22_B59C;
    pub const LANGUAGE_BCP47: u32 = 0x22_B59D;
    pub const CODEC_ID: u32 = 0x86;
    pub const CODEC_PRIVATE: u32 = 0x63A2;
    pub const CODEC_DELAY: u32 = 0x56AA;
    pub const VIDEO: u32 = 0xE0;
    pub const PIXEL_WIDTH: u32 = 0xB0;
    pub const PIXEL_HEIGHT: u32 = 0xBA;
    pub const DISPLAY_WIDTH: u32 = 0x54B0;
    pub const DISPLAY_HEIGHT: u32 = 0x54BA;
    pub const DISPLAY_UNIT: u32 = 0x54B2;
    pub const COLOUR: u32 = 0x55B0;
    pub const MATRIX: u32 = 0x55B1;
    pub const BITS_PER_CHANNEL: u32 = 0x55B2;
    pub const RANGE: u32 = 0x55B9;
    pub const TRANSFER: u32 = 0x55BA;
    pub const PRIMARIES: u32 = 0x55BB;
    pub const AUDIO: u32 = 0xE1;
    pub const SAMPLING_FREQUENCY: u32 = 0xB5;
    pub const CHANNELS: u32 = 0x9F;
    pub const BIT_DEPTH: u32 = 0x6264;
    pub const CONTENT_ENCODINGS: u32 = 0x6D80;
    pub const CONTENT_ENCODING: u32 = 0x6240;
    pub const CONTENT_COMPRESSION: u32 = 0x5034;
    pub const CONTENT_COMP_ALGO: u32 = 0x4254;
    pub const CONTENT_COMP_SETTINGS: u32 = 0x4255;
    pub const CONTENT_ENCRYPTION: u32 = 0x5035;
    pub const CUES: u32 = 0x1C53_BB6B;
    pub const CUE_POINT: u32 = 0xBB;
    pub const CUE_TIME: u32 = 0xB3;
    pub const CUE_TRACK_POSITIONS: u32 = 0xB7;
    pub const CUE_TRACK: u32 = 0xF7;
    pub const CUE_CLUSTER_POSITION: u32 = 0xF1;
    pub const CLUSTER: u32 = 0x1F43_B675;
    pub const TIMESTAMP: u32 = 0xE7;
    pub const SIMPLE_BLOCK: u32 = 0xA3;
    pub const BLOCK_GROUP: u32 = 0xA0;
    pub const BLOCK: u32 = 0xA1;
    pub const BLOCK_DURATION: u32 = 0x9B;
    pub const REFERENCE_BLOCK: u32 = 0xFB;
    pub const TAGS: u32 = 0x1254_C367;
    pub const CHAPTERS: u32 = 0x1043_A770;
    pub const ATTACHMENTS: u32 = 0x1941_A469;
}

/// An element's size when the muxer did not know it (live streams).
const UNKNOWN: u64 = u64::MAX;

/// The largest element read whole into memory (codec private data, a
/// block): anything larger is a damaged file, not media.
const MAX_ELEMENT: u64 = 256 << 20;

/// Elements that may follow a cluster of unknown size and so end it.
fn is_top_level(element: u32) -> bool {
    matches!(
        element,
        id::CLUSTER
            | id::CUES
            | id::TAGS
            | id::CHAPTERS
            | id::ATTACHMENTS
            | id::SEEK_HEAD
            | id::INFO
            | id::TRACKS
    )
}

/// A reader of EBML elements over any seekable byte source.
pub(crate) struct Ebml<R> {
    inner: R,
    position: u64,
}

impl<R: Read + Seek> Ebml<R> {
    pub(crate) fn new(mut inner: R) -> io::Result<Self> {
        let position = inner.stream_position()?;
        Ok(Self { inner, position })
    }

    fn byte(&mut self) -> io::Result<u8> {
        let mut byte = [0u8];
        self.inner.read_exact(&mut byte)?;
        self.position += 1;
        Ok(byte[0])
    }

    /// A variable-length integer. `keep_marker` keeps the length bits, as
    /// element IDs are written; sizes drop them.
    fn vint(&mut self, keep_marker: bool) -> Result<(u64, u32), RemuxError> {
        let first = self.byte()?;
        let length = first.leading_zeros() + 1;
        if length > 8 {
            return Err(RemuxError::invalid(
                "a variable-length integer has no length",
            ));
        }
        let mut value = if keep_marker {
            u64::from(first)
        } else {
            u64::from(first) & (0xFF >> length)
        };
        let mut all_ones = value == (0xFF >> length);
        for _ in 1..length {
            let byte = self.byte()?;
            all_ones &= byte == 0xFF;
            value = (value << 8) | u64::from(byte);
        }
        if !keep_marker && all_ones {
            value = UNKNOWN;
        }
        Ok((value, length))
    }

    /// The next element's ID and data size; the reader is left at its data.
    pub(crate) fn header(&mut self) -> Result<(u32, u64), RemuxError> {
        let (element, length) = self.vint(true)?;
        if length > 4 {
            return Err(RemuxError::invalid("an element ID is longer than 4 bytes"));
        }
        let (size, _) = self.vint(false)?;
        Ok((element as u32, size))
    }

    pub(crate) fn position(&self) -> u64 {
        self.position
    }

    pub(crate) fn seek(&mut self, position: u64) -> io::Result<()> {
        if position != self.position {
            self.inner.seek(SeekFrom::Start(position))?;
            self.position = position;
        }
        Ok(())
    }

    fn skip(&mut self, size: u64) -> io::Result<()> {
        self.seek(self.position + size)
    }

    pub(crate) fn bytes(&mut self, size: u64) -> Result<Vec<u8>, RemuxError> {
        if size > MAX_ELEMENT {
            return Err(RemuxError::invalid("an element is too large to be media"));
        }
        let mut data = vec![0u8; size as usize];
        self.inner.read_exact(&mut data)?;
        self.position += size;
        Ok(data)
    }

    fn uint(&mut self, size: u64) -> Result<u64, RemuxError> {
        if size > 8 {
            return Err(RemuxError::invalid(
                "an unsigned integer is longer than 8 bytes",
            ));
        }
        Ok(self
            .bytes(size)?
            .iter()
            .fold(0u64, |value, byte| (value << 8) | u64::from(*byte)))
    }

    fn float(&mut self, size: u64) -> Result<f64, RemuxError> {
        let data = self.bytes(size)?;
        match data.len() {
            0 => Ok(0.0),
            4 => Ok(f64::from(f32::from_be_bytes([
                data[0], data[1], data[2], data[3],
            ]))),
            8 => Ok(f64::from_be_bytes([
                data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
            ])),
            _ => Err(RemuxError::invalid("a float is neither 4 nor 8 bytes")),
        }
    }

    fn string(&mut self, size: u64) -> Result<String, RemuxError> {
        let data = self.bytes(size)?;
        let end = data
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(data.len());
        Ok(String::from_utf8_lossy(&data[..end]).into_owned())
    }

    /// Calls `each` for every child of a master element of `size` bytes,
    /// with the child's ID and size; `each` must read or skip exactly the
    /// child's data, which `children` checks.
    fn children(
        &mut self,
        size: u64,
        mut each: impl FnMut(&mut Self, u32, u64) -> Result<(), RemuxError>,
    ) -> Result<(), RemuxError> {
        if size == UNKNOWN {
            return Err(RemuxError::invalid("a header element has an unknown size"));
        }
        let end = self.position + size;
        while self.position < end {
            let (element, child_size) = self.header()?;
            if child_size == UNKNOWN {
                return Err(RemuxError::invalid("a header element has an unknown size"));
            }
            let child_end = self.position + child_size;
            if child_end > end {
                return Err(RemuxError::invalid("an element runs past its parent"));
            }
            each(self, element, child_size)?;
            self.seek(child_end)?;
        }
        Ok(())
    }
}

/// What kind of track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Video,
    Audio,
    Other,
}

/// How a video track encodes colour, as Matroska numbers them (the same
/// numbers as ISO/IEC 23091-2, which MP4's `colr` box uses).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Colour {
    pub matrix: Option<u8>,
    pub transfer: Option<u8>,
    pub primaries: Option<u8>,
    /// 1 limited, 2 full.
    pub range: Option<u8>,
    pub bits_per_channel: Option<u8>,
}

#[derive(Clone, Debug)]
pub(crate) struct Track {
    pub number: u64,
    pub kind: Kind,
    pub enabled: bool,
    pub default: bool,
    pub codec_id: String,
    pub codec_private: Vec<u8>,
    /// Nanoseconds per frame, when constant.
    pub default_duration: Option<u64>,
    pub codec_delay: u64,
    pub language: Option<String>,
    pub name: Option<String>,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub display_width: Option<u32>,
    pub display_height: Option<u32>,
    pub colour: Colour,
    pub sample_rate: f64,
    pub channels: u32,
    pub bit_depth: Option<u32>,
    /// Bytes every frame of this track had stripped by the muxer, to put
    /// back (Matroska header stripping).
    pub stripped: Vec<u8>,
}

impl Track {
    fn new() -> Self {
        Self {
            number: 0,
            kind: Kind::Other,
            enabled: true,
            default: true,
            codec_id: String::new(),
            codec_private: Vec::new(),
            default_duration: None,
            codec_delay: 0,
            language: None,
            name: None,
            pixel_width: 0,
            pixel_height: 0,
            display_width: None,
            display_height: None,
            colour: Colour::default(),
            sample_rate: 8000.0,
            channels: 1,
            bit_depth: None,
            stripped: Vec::new(),
        }
    }
}

/// A cue: a time (in timestamp-scale units) and the position of the
/// cluster holding it, from the segment's data start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cue {
    pub time: u64,
    pub track: u64,
    pub cluster: u64,
}

/// A frame of one track, with its presentation time in nanoseconds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Block {
    pub track: u64,
    pub time_ns: i64,
    pub keyframe: bool,
    pub data: Vec<u8>,
}

/// A Matroska file's header: everything before the media data.
#[derive(Debug)]
pub(crate) struct Header {
    pub doc_type: String,
    /// Where the segment's data starts; cue and seek positions count
    /// from here.
    pub segment_start: u64,
    /// Where the segment ends, when its size is known.
    pub segment_end: Option<u64>,
    pub timestamp_scale: u64,
    /// In timestamp-scale units.
    pub duration: Option<f64>,
    pub tracks: Vec<Track>,
    pub cues: Vec<Cue>,
    /// The first cluster's position in the file.
    pub first_cluster: u64,
}

impl Header {
    pub(crate) fn read<R: Read + Seek>(ebml: &mut Ebml<R>) -> Result<Self, RemuxError> {
        let (element, size) = ebml.header()?;
        if element != id::EBML {
            return Err(RemuxError::NotMatroska);
        }
        let mut doc_type = String::from("matroska");
        ebml.children(size, |ebml, element, size| {
            if element == id::DOC_TYPE {
                doc_type = ebml.string(size)?;
            }
            Ok(())
        })?;
        if doc_type != "matroska" && doc_type != "webm" {
            return Err(RemuxError::NotMatroska);
        }
        let (element, segment_size) = ebml.header()?;
        if element != id::SEGMENT {
            return Err(RemuxError::invalid("no segment after the EBML header"));
        }
        let segment_start = ebml.position();
        let segment_end = (segment_size != UNKNOWN).then(|| segment_start + segment_size);
        let mut header = Self {
            doc_type,
            segment_start,
            segment_end,
            timestamp_scale: 1_000_000,
            duration: None,
            tracks: Vec::new(),
            cues: Vec::new(),
            first_cluster: 0,
        };
        let mut seeks: Vec<(u32, u64)> = Vec::new();
        let mut seen_tracks = false;
        let mut seen_cues = false;
        // The top-level elements up to the first cluster.
        loop {
            let start = ebml.position();
            if segment_end.is_some_and(|end| start >= end) {
                break;
            }
            let (element, size) = match ebml.header() {
                Ok(header) => header,
                Err(RemuxError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(error) => return Err(error),
            };
            if element == id::CLUSTER {
                header.first_cluster = start;
                break;
            }
            if size == UNKNOWN {
                return Err(RemuxError::invalid(
                    "a top-level element has an unknown size",
                ));
            }
            let end = ebml.position() + size;
            match element {
                id::SEEK_HEAD => read_seek_head(ebml, size, &mut seeks)?,
                id::INFO => header.read_info(ebml, size)?,
                id::TRACKS => {
                    header.read_tracks(ebml, size)?;
                    seen_tracks = true;
                }
                id::CUES => {
                    header.read_cues(ebml, size)?;
                    seen_cues = true;
                }
                _ => {}
            }
            ebml.seek(end)?;
        }
        if header.first_cluster == 0 {
            return Err(RemuxError::invalid("the file has no clusters"));
        }
        // Tracks and cues written after the clusters, found through the
        // seek head.
        for (element, position) in seeks {
            let wanted = match element {
                id::TRACKS => !seen_tracks,
                id::CUES => !seen_cues,
                _ => false,
            };
            if !wanted {
                continue;
            }
            ebml.seek(segment_start + position)?;
            let (found, size) = ebml.header()?;
            if found != element || size == UNKNOWN {
                continue;
            }
            match element {
                id::TRACKS => {
                    header.read_tracks(ebml, size)?;
                    seen_tracks = true;
                }
                _ => {
                    // Damaged cues are not worth failing over: the file
                    // can be scanned instead.
                    if header.read_cues(ebml, size).is_err() {
                        header.cues.clear();
                    }
                }
            }
        }
        if !seen_tracks || header.tracks.is_empty() {
            return Err(RemuxError::invalid("the file lists no tracks"));
        }
        header.cues.sort_by_key(|cue| (cue.time, cue.cluster));
        Ok(header)
    }

    fn read_info<R: Read + Seek>(
        &mut self,
        ebml: &mut Ebml<R>,
        size: u64,
    ) -> Result<(), RemuxError> {
        ebml.children(size, |ebml, element, size| {
            match element {
                id::TIMESTAMP_SCALE => self.timestamp_scale = ebml.uint(size)?.max(1),
                id::DURATION => self.duration = Some(ebml.float(size)?),
                _ => {}
            }
            Ok(())
        })
    }

    fn read_tracks<R: Read + Seek>(
        &mut self,
        ebml: &mut Ebml<R>,
        size: u64,
    ) -> Result<(), RemuxError> {
        let mut tracks = Vec::new();
        ebml.children(size, |ebml, element, size| {
            if element == id::TRACK_ENTRY {
                tracks.push(read_track(ebml, size)?);
            }
            Ok(())
        })?;
        self.tracks = tracks;
        Ok(())
    }

    fn read_cues<R: Read + Seek>(
        &mut self,
        ebml: &mut Ebml<R>,
        size: u64,
    ) -> Result<(), RemuxError> {
        let mut cues = Vec::new();
        ebml.children(size, |ebml, element, size| {
            if element != id::CUE_POINT {
                return Ok(());
            }
            let mut time = None;
            let mut positions = Vec::new();
            ebml.children(size, |ebml, element, size| {
                match element {
                    id::CUE_TIME => time = Some(ebml.uint(size)?),
                    id::CUE_TRACK_POSITIONS => {
                        let (mut track, mut cluster) = (None, None);
                        ebml.children(size, |ebml, element, size| {
                            match element {
                                id::CUE_TRACK => track = Some(ebml.uint(size)?),
                                id::CUE_CLUSTER_POSITION => cluster = Some(ebml.uint(size)?),
                                _ => {}
                            }
                            Ok(())
                        })?;
                        if let (Some(track), Some(cluster)) = (track, cluster) {
                            positions.push((track, cluster));
                        }
                    }
                    _ => {}
                }
                Ok(())
            })?;
            if let Some(time) = time {
                cues.extend(positions.into_iter().map(|(track, cluster)| Cue {
                    time,
                    track,
                    cluster,
                }));
            }
            Ok(())
        })?;
        self.cues = cues;
        Ok(())
    }

    pub(crate) fn track(&self, number: u64) -> Option<&Track> {
        self.tracks.iter().find(|track| track.number == number)
    }
}

fn read_seek_head<R: Read + Seek>(
    ebml: &mut Ebml<R>,
    size: u64,
    seeks: &mut Vec<(u32, u64)>,
) -> Result<(), RemuxError> {
    ebml.children(size, |ebml, element, size| {
        if element != id::SEEK {
            return Ok(());
        }
        let (mut target, mut position) = (None, None);
        ebml.children(size, |ebml, element, size| {
            match element {
                id::SEEK_ID => target = Some(ebml.uint(size)? as u32),
                id::SEEK_POSITION => position = Some(ebml.uint(size)?),
                _ => {}
            }
            Ok(())
        })?;
        if let (Some(target), Some(position)) = (target, position) {
            seeks.push((target, position));
        }
        Ok(())
    })
}

fn read_track<R: Read + Seek>(ebml: &mut Ebml<R>, size: u64) -> Result<Track, RemuxError> {
    let mut track = Track::new();
    let mut language_bcp47 = None;
    ebml.children(size, |ebml, element, size| {
        match element {
            id::TRACK_NUMBER => track.number = ebml.uint(size)?,
            id::TRACK_TYPE => {
                track.kind = match ebml.uint(size)? {
                    1 => Kind::Video,
                    2 => Kind::Audio,
                    _ => Kind::Other,
                }
            }
            id::FLAG_ENABLED => track.enabled = ebml.uint(size)? != 0,
            id::FLAG_DEFAULT => track.default = ebml.uint(size)? != 0,
            id::DEFAULT_DURATION => {
                track.default_duration = Some(ebml.uint(size)?).filter(|value| *value > 0)
            }
            id::CODEC_DELAY => track.codec_delay = ebml.uint(size)?,
            id::NAME => track.name = Some(ebml.string(size)?).filter(|name| !name.is_empty()),
            id::LANGUAGE => track.language = Some(ebml.string(size)?),
            id::LANGUAGE_BCP47 => language_bcp47 = Some(ebml.string(size)?),
            id::CODEC_ID => track.codec_id = ebml.string(size)?,
            id::CODEC_PRIVATE => track.codec_private = ebml.bytes(size)?,
            id::VIDEO => read_video(ebml, size, &mut track)?,
            id::AUDIO => ebml.children(size, |ebml, element, size| {
                match element {
                    id::SAMPLING_FREQUENCY => track.sample_rate = ebml.float(size)?,
                    id::CHANNELS => track.channels = ebml.uint(size)?.clamp(1, 255) as u32,
                    id::BIT_DEPTH => track.bit_depth = Some(ebml.uint(size)? as u32),
                    _ => {}
                }
                Ok(())
            })?,
            id::CONTENT_ENCODINGS => read_encodings(ebml, size, &mut track)?,
            _ => {}
        }
        Ok(())
    })?;
    if language_bcp47.is_some() {
        track.language = language_bcp47;
    }
    if track.language.as_deref() == Some("und") {
        track.language = None;
    }
    Ok(track)
}

fn read_video<R: Read + Seek>(
    ebml: &mut Ebml<R>,
    size: u64,
    track: &mut Track,
) -> Result<(), RemuxError> {
    let mut display_unit = 0;
    ebml.children(size, |ebml, element, size| {
        match element {
            id::PIXEL_WIDTH => track.pixel_width = ebml.uint(size)? as u32,
            id::PIXEL_HEIGHT => track.pixel_height = ebml.uint(size)? as u32,
            id::DISPLAY_WIDTH => track.display_width = Some(ebml.uint(size)? as u32),
            id::DISPLAY_HEIGHT => track.display_height = Some(ebml.uint(size)? as u32),
            id::DISPLAY_UNIT => display_unit = ebml.uint(size)?,
            id::COLOUR => ebml.children(size, |ebml, element, size| {
                match element {
                    id::MATRIX => track.colour.matrix = Some(ebml.uint(size)? as u8),
                    id::TRANSFER => track.colour.transfer = Some(ebml.uint(size)? as u8),
                    id::PRIMARIES => track.colour.primaries = Some(ebml.uint(size)? as u8),
                    id::RANGE => track.colour.range = Some(ebml.uint(size)? as u8),
                    id::BITS_PER_CHANNEL => {
                        track.colour.bits_per_channel = Some(ebml.uint(size)? as u8)
                    }
                    _ => {}
                }
                Ok(())
            })?,
            _ => {}
        }
        Ok(())
    })?;
    // Display sizes in centimetres, inches or as a bare ratio say only the
    // shape: keep them as a ratio over the pixel size.
    if display_unit != 0
        && let (Some(width), Some(height)) = (track.display_width, track.display_height)
        && width > 0
        && height > 0
    {
        let shown_width =
            (u64::from(track.pixel_height) * u64::from(width) / u64::from(height)) as u32;
        track.display_width = Some(shown_width);
        track.display_height = Some(track.pixel_height);
    }
    Ok(())
}

fn read_encodings<R: Read + Seek>(
    ebml: &mut Ebml<R>,
    size: u64,
    track: &mut Track,
) -> Result<(), RemuxError> {
    ebml.children(size, |ebml, element, size| {
        if element != id::CONTENT_ENCODING {
            return Ok(());
        }
        ebml.children(size, |ebml, element, size| {
            match element {
                id::CONTENT_ENCRYPTION => {
                    return Err(RemuxError::Unsupported("encrypted tracks".into()));
                }
                id::CONTENT_COMPRESSION => {
                    let mut algorithm = 0;
                    let mut settings = Vec::new();
                    ebml.children(size, |ebml, element, size| {
                        match element {
                            id::CONTENT_COMP_ALGO => algorithm = ebml.uint(size)?,
                            id::CONTENT_COMP_SETTINGS => settings = ebml.bytes(size)?,
                            _ => {}
                        }
                        Ok(())
                    })?;
                    // 3 is header stripping; zlib, bzlib and lzo are rare
                    // and not supported.
                    if algorithm != 3 {
                        return Err(RemuxError::Unsupported("compressed tracks".into()));
                    }
                    track.stripped = settings;
                }
                _ => {}
            }
            Ok(())
        })
    })
}

/// A cluster's position and time, from a scan of a file without cues.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ClusterStart {
    pub position: u64,
    /// In timestamp-scale units.
    pub time: u64,
    /// Whether the cluster's first block of `track` is a keyframe.
    pub starts_with_keyframe: bool,
}

/// Reads the clusters from `position` until `end` (a position, exclusive)
/// or the end of the segment, and passes each block to `each`. Laced
/// blocks come out as one block per frame.
pub(crate) fn read_clusters<R: Read + Seek>(
    ebml: &mut Ebml<R>,
    header: &Header,
    position: u64,
    end: Option<u64>,
    mut each: impl FnMut(Block),
) -> Result<(), RemuxError> {
    ebml.seek(position)?;
    loop {
        let start = ebml.position();
        if end.is_some_and(|end| start >= end) || header.segment_end.is_some_and(|end| start >= end)
        {
            return Ok(());
        }
        let (element, size) = match ebml.header() {
            Ok(header) => header,
            Err(RemuxError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof => {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if element != id::CLUSTER {
            if size == UNKNOWN {
                return Ok(());
            }
            ebml.skip(size)?;
            continue;
        }
        read_cluster(ebml, header, size, &mut each)?;
    }
}

/// Lists each cluster's position, time and whether it starts with a
/// keyframe of `track`, reading only block headers.
pub(crate) fn scan_clusters<R: Read + Seek>(
    ebml: &mut Ebml<R>,
    header: &Header,
    track: u64,
) -> Result<Vec<ClusterStart>, RemuxError> {
    let mut clusters = Vec::new();
    ebml.seek(header.first_cluster)?;
    loop {
        let start = ebml.position();
        if header.segment_end.is_some_and(|end| start >= end) {
            break;
        }
        let (element, size) = match ebml.header() {
            Ok(header) => header,
            Err(RemuxError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error),
        };
        if element != id::CLUSTER {
            if size == UNKNOWN {
                break;
            }
            ebml.skip(size)?;
            continue;
        }
        let end = (size != UNKNOWN).then(|| ebml.position() + size);
        let mut time = 0;
        let mut first_key = None;
        loop {
            let child = ebml.position();
            if end.is_some_and(|end| child >= end) {
                break;
            }
            let (element, child_size) = match ebml.header() {
                Ok(header) => header,
                Err(RemuxError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(error) => return Err(error),
            };
            if end.is_none() && is_top_level(element) {
                ebml.seek(child)?;
                break;
            }
            let data_end = ebml.position() + child_size;
            match element {
                id::TIMESTAMP => time = ebml.uint(child_size)?,
                id::SIMPLE_BLOCK if first_key.is_none() => {
                    let (number, _) = ebml.vint(false)?;
                    if number == track {
                        ebml.skip(2)?;
                        first_key = Some(ebml.byte()? & 0x80 != 0);
                    }
                }
                id::BLOCK_GROUP if first_key.is_none() => {
                    let mut is_track = false;
                    let mut referenced = false;
                    ebml.children(child_size, |ebml, element, _| {
                        match element {
                            id::BLOCK => is_track = ebml.vint(false)?.0 == track,
                            id::REFERENCE_BLOCK => referenced = true,
                            _ => {}
                        }
                        Ok(())
                    })?;
                    if is_track {
                        first_key = Some(!referenced);
                    }
                }
                _ => {}
            }
            ebml.seek(data_end)?;
        }
        clusters.push(ClusterStart {
            position: start,
            time,
            starts_with_keyframe: first_key.unwrap_or(false),
        });
        if let Some(end) = end {
            ebml.seek(end)?;
        }
    }
    Ok(clusters)
}

fn read_cluster<R: Read + Seek>(
    ebml: &mut Ebml<R>,
    header: &Header,
    size: u64,
    each: &mut impl FnMut(Block),
) -> Result<(), RemuxError> {
    let end = (size != UNKNOWN).then(|| ebml.position() + size);
    let mut cluster_time: u64 = 0;
    loop {
        let child = ebml.position();
        if end.is_some_and(|end| child >= end) {
            return Ok(());
        }
        let (element, child_size) = match ebml.header() {
            Ok(header) => header,
            Err(RemuxError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof => {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if end.is_none() && is_top_level(element) {
            ebml.seek(child)?;
            return Ok(());
        }
        if child_size == UNKNOWN {
            return Err(RemuxError::invalid("a block has an unknown size"));
        }
        let data_end = ebml.position() + child_size;
        match element {
            id::TIMESTAMP => cluster_time = ebml.uint(child_size)?,
            id::SIMPLE_BLOCK => {
                let data = ebml.bytes(child_size)?;
                split_block(header, cluster_time, &data, None, each)?;
            }
            id::BLOCK_GROUP => {
                let mut data = None;
                let mut referenced = false;
                ebml.children(child_size, |ebml, element, size| {
                    match element {
                        id::BLOCK => data = Some(ebml.bytes(size)?),
                        id::REFERENCE_BLOCK => referenced = true,
                        id::BLOCK_DURATION => {}
                        _ => {}
                    }
                    Ok(())
                })?;
                if let Some(data) = data {
                    split_block(header, cluster_time, &data, Some(!referenced), each)?;
                }
            }
            _ => {}
        }
        ebml.seek(data_end)?;
    }
}

/// Reads a block's header and frames. `keyframe` is set for a Block in a
/// group (from its references); a SimpleBlock says so in its flags.
fn split_block(
    header: &Header,
    cluster_time: u64,
    data: &[u8],
    keyframe: Option<bool>,
    each: &mut impl FnMut(Block),
) -> Result<(), RemuxError> {
    let mut cursor = io::Cursor::new(data);
    let (number, length) = Ebml::new(&mut cursor)?.vint(false)?;
    let mut at = length as usize;
    let Some(rest) = data.get(at..at + 3) else {
        return Err(RemuxError::invalid("a block is shorter than its header"));
    };
    let relative = i16::from_be_bytes([rest[0], rest[1]]);
    let flags = rest[2];
    at += 3;
    let Some(track) = header.track(number) else {
        // A block of a track the header does not list.
        return Ok(());
    };
    let keyframe = keyframe.unwrap_or(flags & 0x80 != 0);
    let scale = header.timestamp_scale as i64;
    let time_ns = (cluster_time as i64 + i64::from(relative)) * scale;
    let frames = lace(flags, &data[at..])?;
    let step = track.default_duration.unwrap_or(0) as i64;
    for (index, frame) in frames.into_iter().enumerate() {
        let mut bytes = Vec::with_capacity(track.stripped.len() + frame.len());
        bytes.extend_from_slice(&track.stripped);
        bytes.extend_from_slice(frame);
        each(Block {
            track: number,
            time_ns: time_ns + index as i64 * step,
            keyframe: keyframe && (index == 0 || track.kind == Kind::Audio),
            data: bytes,
        });
    }
    Ok(())
}

/// Splits a block's payload into its frames, by its lacing.
fn lace(flags: u8, payload: &[u8]) -> Result<Vec<&[u8]>, RemuxError> {
    let lacing = (flags >> 1) & 0x03;
    if lacing == 0 {
        return Ok(vec![payload]);
    }
    let bad = || RemuxError::invalid("a laced block is damaged");
    let count = usize::from(*payload.first().ok_or_else(bad)?) + 1;
    let mut at = 1;
    let mut sizes = Vec::with_capacity(count);
    match lacing {
        // Xiph: each size as a run of 255s and a final byte.
        1 => {
            for _ in 0..count - 1 {
                let mut size = 0usize;
                loop {
                    let byte = *payload.get(at).ok_or_else(bad)?;
                    at += 1;
                    size += usize::from(byte);
                    if byte != 255 {
                        break;
                    }
                }
                sizes.push(size);
            }
        }
        // Fixed: equal sizes.
        2 => {
            let total = payload.len() - at;
            if !total.is_multiple_of(count) {
                return Err(bad());
            }
            sizes.extend(std::iter::repeat_n(total / count, count - 1));
        }
        // EBML: the first size, then signed differences.
        _ => {
            let mut cursor = io::Cursor::new(&payload[at..]);
            let mut reader = Ebml::new(&mut cursor)?;
            let (first, _) = reader.vint(false)?;
            let mut size = first as i64;
            sizes.push(first as usize);
            for _ in 1..count - 1 {
                let (raw, length) = reader.vint(false)?;
                let bias = (1i64 << (7 * length - 1)) - 1;
                size += raw as i64 - bias;
                if size < 0 {
                    return Err(bad());
                }
                sizes.push(size as usize);
            }
            at += reader.position() as usize;
        }
    }
    let used: usize = sizes.iter().sum();
    let last = (payload.len() - at).checked_sub(used).ok_or_else(bad)?;
    sizes.push(last);
    let mut frames = Vec::with_capacity(count);
    for size in sizes {
        frames.push(payload.get(at..at + size).ok_or_else(bad)?);
        at += size;
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::lace;

    #[test]
    fn xiph_lacing_splits_by_runs_of_255() {
        // Three frames: 300, 2 and the rest (1) bytes.
        let mut payload = vec![2u8, 255, 45, 2];
        payload.extend(std::iter::repeat_n(7u8, 303));
        let frames = lace(0b0000_0010, &payload).unwrap();
        let sizes: Vec<_> = frames.iter().map(|frame| frame.len()).collect();
        assert_eq!(sizes, [300, 2, 1]);
    }

    #[test]
    fn fixed_lacing_splits_evenly() {
        let payload = [1u8, 1, 2, 3, 4];
        let frames = lace(0b0000_0100, &payload).unwrap();
        assert_eq!(frames, [&[1u8, 2][..], &[3, 4][..]]);
    }

    #[test]
    fn ebml_lacing_reads_signed_differences() {
        // Three frames of 3, 2 and 4 bytes: first size 3 (0x83), then a
        // difference of -1 as a 1-byte signed vint (bias 63: 62 = 0xBE).
        let mut payload = vec![2u8, 0x83, 0xBE];
        payload.extend([1, 1, 1, 2, 2, 3, 3, 3, 3]);
        let frames = lace(0b0000_0110, &payload).unwrap();
        let sizes: Vec<_> = frames.iter().map(|frame| frame.len()).collect();
        assert_eq!(sizes, [3, 2, 4]);
    }

    #[test]
    fn damaged_lacing_is_an_error_not_a_panic() {
        assert!(lace(0b0000_0010, &[5u8, 255]).is_err());
        assert!(lace(0b0000_0100, &[1u8, 1, 2, 3]).is_err());
    }
}
