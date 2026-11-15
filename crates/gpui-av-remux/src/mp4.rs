//! Writing fragmented MP4: an init segment (`ftyp` and `moov`) and media
//! segments (`moof` and `mdat`), as HLS and Media Source Extensions take
//! them. Also the RFC 6381 codec strings both need.

use crate::RemuxError;
use crate::matroska::{Colour, Kind, Track};

/// Builds one box: its size, its type, then what `body` writes.
fn boxed(kind: &[u8; 4], body: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut out = vec![0, 0, 0, 0];
    out.extend_from_slice(kind);
    body(&mut out);
    let size = out.len() as u32;
    out[..4].copy_from_slice(&size.to_be_bytes());
    out
}

/// A full box: a box with a version and 24 bits of flags first.
fn full(kind: &[u8; 4], version: u8, flags: u32, body: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    boxed(kind, |out| {
        out.push(version);
        out.extend_from_slice(&flags.to_be_bytes()[1..]);
        body(out);
    })
}

trait Put {
    fn u8(&mut self, value: u8);
    fn u16(&mut self, value: u16);
    fn u32(&mut self, value: u32);
    fn u64(&mut self, value: u64);
    fn zeros(&mut self, count: usize);
}

impl Put for Vec<u8> {
    fn u8(&mut self, value: u8) {
        self.push(value);
    }
    fn u16(&mut self, value: u16) {
        self.extend_from_slice(&value.to_be_bytes());
    }
    fn u32(&mut self, value: u32) {
        self.extend_from_slice(&value.to_be_bytes());
    }
    fn u64(&mut self, value: u64) {
        self.extend_from_slice(&value.to_be_bytes());
    }
    fn zeros(&mut self, count: usize) {
        self.extend(std::iter::repeat_n(0u8, count));
    }
}

/// The identity matrix of `mvhd` and `tkhd`.
const MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

/// A track as it is written to MP4.
#[derive(Clone, Debug)]
pub(crate) struct Output {
    /// The MP4 track ID, from 1.
    pub id: u32,
    /// The Matroska track number it comes from.
    pub source: u64,
    pub kind: Kind,
    pub timescale: u32,
    /// The sample entry (`avc1`, `mp4a` and so on), written once.
    pub entry: Vec<u8>,
    /// The RFC 6381 codec string.
    pub codec: String,
    pub width: u32,
    pub height: u32,
    /// The picture's size as shown, after non-square pixels.
    pub display: (u32, u32),
    pub language: Option<String>,
    /// Nanoseconds per frame, for the last frame of a segment.
    pub default_duration: Option<u64>,
}

impl Output {
    /// The sample entry and codec string for a Matroska track, or why it
    /// cannot go into MP4.
    pub(crate) fn new(
        id: u32,
        track: &Track,
        first_frame: Option<&[u8]>,
    ) -> Result<Self, RemuxError> {
        let private = &track.codec_private;
        let unsupported = || RemuxError::Unsupported(format!("the {} codec", track.codec_id));
        let (entry, codec, timescale) = match track.kind {
            Kind::Video => {
                let (kind, config, codec) = match track.codec_id.as_str() {
                    "V_MPEG4/ISO/AVC" => {
                        let codec = avc_codec(private).ok_or_else(unsupported)?;
                        (
                            b"avc1",
                            boxed(b"avcC", |out| out.extend_from_slice(private)),
                            codec,
                        )
                    }
                    "V_MPEGH/ISO/HEVC" => {
                        let codec = hevc_codec(private).ok_or_else(unsupported)?;
                        (
                            b"hvc1",
                            boxed(b"hvcC", |out| out.extend_from_slice(private)),
                            codec,
                        )
                    }
                    "V_AV1" => {
                        let codec = av1_codec(private).ok_or_else(unsupported)?;
                        (
                            b"av01",
                            boxed(b"av1C", |out| out.extend_from_slice(private)),
                            codec,
                        )
                    }
                    "V_VP9" => {
                        let (config, codec) = vp9_config(track, first_frame);
                        (b"vp09", config, codec)
                    }
                    _ => return Err(unsupported()),
                };
                (visual_entry(kind, track, &config), codec, 90_000)
            }
            Kind::Audio => {
                let rate = track.sample_rate.round().clamp(1.0, f64::from(u32::MAX)) as u32;
                let (kind, config, codec, rate) = match track.codec_id.as_str() {
                    "A_AAC" => {
                        let object = private
                            .first()
                            .map(|byte| byte >> 3)
                            .filter(|object| *object > 0);
                        let codec = format!("mp4a.40.{}", object.ok_or_else(unsupported)?);
                        (b"mp4a", esds(private), codec, rate)
                    }
                    "A_OPUS" => (
                        b"Opus",
                        dops(private).ok_or_else(unsupported)?,
                        "opus".to_string(),
                        48_000,
                    ),
                    "A_FLAC" => (
                        b"fLaC",
                        dfla(private).ok_or_else(unsupported)?,
                        "flac".to_string(),
                        rate,
                    ),
                    _ => return Err(unsupported()),
                };
                (audio_entry(kind, track, rate, &config), codec, rate)
            }
            Kind::Other => return Err(RemuxError::Unsupported("text tracks".into())),
        };
        let display = (
            track
                .display_width
                .filter(|width| *width > 0)
                .unwrap_or(track.pixel_width),
            track
                .display_height
                .filter(|height| *height > 0)
                .unwrap_or(track.pixel_height),
        );
        Ok(Self {
            id,
            source: track.number,
            kind: track.kind,
            timescale,
            entry,
            codec,
            width: track.pixel_width,
            height: track.pixel_height,
            display,
            language: track.language.clone(),
            default_duration: track.default_duration,
        })
    }

    pub(crate) fn ticks(&self, nanoseconds: i64) -> i64 {
        (i128::from(nanoseconds) * i128::from(self.timescale) / 1_000_000_000) as i64
    }
}

fn visual_entry(kind: &[u8; 4], track: &Track, config: &[u8]) -> Vec<u8> {
    boxed(kind, |out| {
        out.zeros(6);
        out.u16(1); // data reference index
        out.zeros(16);
        out.u16(track.pixel_width.min(u32::from(u16::MAX)) as u16);
        out.u16(track.pixel_height.min(u32::from(u16::MAX)) as u16);
        out.u32(0x0048_0000);
        out.u32(0x0048_0000);
        out.u32(0);
        out.u16(1); // frame count
        out.zeros(32); // compressor name
        out.u16(0x0018);
        out.u16(0xFFFF);
        out.extend_from_slice(config);
        if let Some(colr) = colr(&track.colour) {
            out.extend_from_slice(&colr);
        }
        // Non-square pixels.
        if let (Some(width), Some(height)) = (track.display_width, track.display_height)
            && width > 0
            && height > 0
            && track.pixel_width > 0
            && track.pixel_height > 0
            && u64::from(width) * u64::from(track.pixel_height)
                != u64::from(height) * u64::from(track.pixel_width)
        {
            let h = u64::from(width) * u64::from(track.pixel_height);
            let v = u64::from(height) * u64::from(track.pixel_width);
            let divisor = gcd(h, v);
            out.extend_from_slice(&boxed(b"pasp", |out| {
                out.u32((h / divisor).min(u64::from(u32::MAX)) as u32);
                out.u32((v / divisor).min(u64::from(u32::MAX)) as u32);
            }));
        }
    })
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

/// The `colr` box, when the track states its colour. Matroska and MP4
/// number primaries, transfer and matrix the same way (ISO/IEC 23091-2).
fn colr(colour: &Colour) -> Option<Vec<u8>> {
    let stated = |value: Option<u8>| value.filter(|value| *value != 2);
    if stated(colour.primaries).is_none()
        && stated(colour.transfer).is_none()
        && stated(colour.matrix).is_none()
        && colour.range.is_none()
    {
        return None;
    }
    Some(boxed(b"colr", |out| {
        out.extend_from_slice(b"nclx");
        out.u16(u16::from(colour.primaries.unwrap_or(2)));
        out.u16(u16::from(colour.transfer.unwrap_or(2)));
        out.u16(u16::from(colour.matrix.unwrap_or(2)));
        out.u8(if colour.range == Some(2) { 0x80 } else { 0 });
    }))
}

fn audio_entry(kind: &[u8; 4], track: &Track, rate: u32, config: &[u8]) -> Vec<u8> {
    boxed(kind, |out| {
        out.zeros(6);
        out.u16(1);
        out.zeros(8);
        out.u16(track.channels.min(u32::from(u16::MAX)) as u16);
        out.u16(16);
        out.u16(0);
        out.u16(0);
        // 16.16 fixed point; rates past 65535 are stated in the config.
        out.u32(if rate <= 0xFFFF { rate << 16 } else { 0 });
        out.extend_from_slice(config);
    })
}

/// An MPEG-4 descriptor: tag, length (in the 4-byte form) and body.
fn descriptor(tag: u8, body: &[u8]) -> Vec<u8> {
    let length = body.len() as u32;
    let mut out = vec![
        tag,
        0x80 | ((length >> 21) & 0x7F) as u8,
        0x80 | ((length >> 14) & 0x7F) as u8,
        0x80 | ((length >> 7) & 0x7F) as u8,
        (length & 0x7F) as u8,
    ];
    out.extend_from_slice(body);
    out
}

fn esds(audio_specific_config: &[u8]) -> Vec<u8> {
    let mut decoder_config = vec![0x40, 0x15, 0, 0, 0];
    decoder_config.extend_from_slice(&[0; 8]);
    decoder_config.extend(descriptor(5, audio_specific_config));
    let mut es = vec![0, 0, 0];
    es.extend(descriptor(4, &decoder_config));
    es.extend(descriptor(6, &[2]));
    full(b"esds", 0, 0, |out| out.extend(descriptor(3, &es)))
}

/// `dOps` from Matroska's `OpusHead` (little-endian; `dOps` is big-endian).
fn dops(head: &[u8]) -> Option<Vec<u8>> {
    if head.len() < 19 || &head[..8] != b"OpusHead" {
        return None;
    }
    let channels = head[9];
    let pre_skip = u16::from_le_bytes([head[10], head[11]]);
    let rate = u32::from_le_bytes([head[12], head[13], head[14], head[15]]);
    let gain = i16::from_le_bytes([head[16], head[17]]);
    let family = head[18];
    let mapping = if family != 0 {
        head.get(19..21 + usize::from(channels))?
    } else {
        &[]
    };
    Some(boxed(b"dOps", |out| {
        out.u8(0);
        out.u8(channels);
        out.u16(pre_skip);
        out.u32(rate);
        out.extend_from_slice(&gain.to_be_bytes());
        out.u8(family);
        out.extend_from_slice(mapping);
    }))
}

/// `dfLa` from Matroska's FLAC private data (`fLaC` and metadata blocks).
fn dfla(private: &[u8]) -> Option<Vec<u8>> {
    let mut blocks = private.strip_prefix(b"fLaC")?.to_vec();
    // Mark the last block as last, as dfLa requires.
    let mut at = 0;
    let mut last = None;
    while at + 4 <= blocks.len() {
        last = Some(at);
        let length =
            u32::from_be_bytes([0, blocks[at + 1], blocks[at + 2], blocks[at + 3]]) as usize;
        if blocks[at] & 0x80 != 0 {
            break;
        }
        at += 4 + length;
    }
    blocks[last?] |= 0x80;
    Some(full(b"dfLa", 0, 0, |out| out.extend_from_slice(&blocks)))
}

/// `avc1.PPCCLL` from an AVCDecoderConfigurationRecord.
fn avc_codec(record: &[u8]) -> Option<String> {
    let bytes = record.get(1..4)?;
    Some(format!(
        "avc1.{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2]
    ))
}

/// `hvc1.<profile>.<compat>.<tier><level>.<constraints>` from an
/// HEVCDecoderConfigurationRecord.
fn hevc_codec(record: &[u8]) -> Option<String> {
    if record.len() < 13 {
        return None;
    }
    let space = match record[1] >> 6 {
        0 => "",
        1 => "A",
        2 => "B",
        _ => "C",
    };
    let tier = if record[1] & 0x20 != 0 { 'H' } else { 'L' };
    let profile = record[1] & 0x1F;
    let compatibility =
        u32::from_be_bytes([record[2], record[3], record[4], record[5]]).reverse_bits();
    let level = record[12];
    let mut constraints: Vec<u8> = record[6..12].to_vec();
    while constraints.last() == Some(&0) {
        constraints.pop();
    }
    let mut codec = format!("hvc1.{space}{profile}.{compatibility:x}.{tier}{level}");
    for byte in constraints {
        codec += &format!(".{byte:x}");
    }
    Some(codec)
}

/// `av01.P.LLT.DD` from an AV1CodecConfigurationRecord.
fn av1_codec(record: &[u8]) -> Option<String> {
    if record.len() < 4 || record[0] & 0x7F != 1 {
        return None;
    }
    let profile = record[1] >> 5;
    let level = record[1] & 0x1F;
    let tier = if record[2] & 0x80 != 0 { 'H' } else { 'M' };
    let depth = match (record[2] & 0x40 != 0, record[2] & 0x20 != 0) {
        (true, true) => 12,
        (true, false) => 10,
        _ => 8,
    };
    Some(format!("av01.{profile}.{level:02}{tier}.{depth:02}"))
}

/// `vpcC` and `vp09.PP.LL.DD`, from Matroska's codec private features when
/// present, else from the first frame's header and the track's colour.
fn vp9_config(track: &Track, first_frame: Option<&[u8]>) -> (Vec<u8>, String) {
    let mut profile = None;
    let mut level = None;
    let mut depth = None;
    // Codec features: ID, length, value.
    let private = &track.codec_private;
    let mut at = 0;
    while at + 2 < private.len() {
        let (feature, length) = (private[at], usize::from(private[at + 1]));
        let Some(&value) = private.get(at + 2) else {
            break;
        };
        match feature {
            1 => profile = Some(value),
            2 => level = Some(value),
            3 => depth = Some(value),
            _ => {}
        }
        at += 2 + length;
    }
    // The uncompressed header: frame marker (2 bits), then the profile's
    // low and high bits.
    let profile = profile.unwrap_or_else(|| {
        first_frame
            .and_then(|frame| frame.first())
            .map(|byte| ((byte >> 5) & 1) | (((byte >> 4) & 1) << 1))
            .unwrap_or(0)
    });
    let depth = depth
        .or(track.colour.bits_per_channel)
        .unwrap_or(if profile >= 2 { 10 } else { 8 });
    // A level the size allows: 3.1 up to 720p, 4.1 to 1080p, 5.1 above.
    let pixels = u64::from(track.pixel_width) * u64::from(track.pixel_height);
    let level = level.unwrap_or(match pixels {
        0..=983_040 => 31,
        983_041..=2_228_224 => 41,
        _ => 51,
    });
    let colour = &track.colour;
    let config = full(b"vpcC", 1, 0, |out| {
        out.u8(profile);
        out.u8(level);
        // Chroma 4:2:0 colocated with luma (1) unless stated otherwise.
        out.u8((depth << 4) | (1 << 1) | u8::from(colour.range == Some(2)));
        out.u8(colour.primaries.unwrap_or(2));
        out.u8(colour.transfer.unwrap_or(2));
        out.u8(colour.matrix.unwrap_or(2));
        out.u16(0);
    });
    (config, format!("vp09.{profile:02}.{level:02}.{depth:02}"))
}

/// Packs an ISO 639-2/T code into `mdhd`'s 15 bits; `und` when unknown.
fn language_code(language: Option<&str>) -> u16 {
    let code = language
        .map(|language| language.split(['-', '_']).next().unwrap_or(language))
        .map(iso_639_2)
        .unwrap_or("und");
    let bytes = code.as_bytes();
    if bytes.len() != 3 || !bytes.iter().all(u8::is_ascii_lowercase) {
        return language_code(None);
    }
    bytes
        .iter()
        .fold(0u16, |packed, byte| (packed << 5) | u16::from(byte - 0x60))
}

/// Two-letter codes as three, for the languages most media carries.
fn iso_639_2(code: &str) -> &str {
    match code {
        "en" => "eng",
        "fr" => "fra",
        "de" => "deu",
        "es" => "spa",
        "it" => "ita",
        "pt" => "por",
        "ja" => "jpn",
        "zh" => "zho",
        "ko" => "kor",
        "ru" => "rus",
        "hi" => "hin",
        "ar" => "ara",
        "nl" => "nld",
        "sv" => "swe",
        "pl" => "pol",
        "tr" => "tur",
        other => other,
    }
}

/// The init segment: `ftyp` and a `moov` with one track per output and
/// `mvex` saying the samples come in fragments.
pub(crate) fn init_segment(outputs: &[Output], duration_ns: Option<u64>) -> Vec<u8> {
    let mut out = boxed(b"ftyp", |out| {
        out.extend_from_slice(b"iso6");
        out.u32(0);
        for brand in [b"iso6", b"isom", b"mp41", b"dash"] {
            out.extend_from_slice(brand);
        }
    });
    let movie_timescale = 1000u32;
    let movie_duration = duration_ns.map_or(0, |duration| duration / 1_000_000);
    let next_id = outputs.iter().map(|output| output.id).max().unwrap_or(0) + 1;
    out.extend(boxed(b"moov", |moov| {
        moov.extend(full(b"mvhd", 1, 0, |out| {
            out.u64(0);
            out.u64(0);
            out.u32(movie_timescale);
            // The length goes in mehd; mvhd states none, as fragmented
            // files do.
            out.u64(0);
            out.u32(0x0001_0000);
            out.u16(0x0100);
            out.zeros(10);
            for value in MATRIX {
                out.u32(value);
            }
            out.zeros(24);
            out.u32(next_id);
        }));
        for output in outputs {
            moov.extend(trak(output));
        }
        moov.extend(boxed(b"mvex", |mvex| {
            if movie_duration > 0 {
                mvex.extend(full(b"mehd", 1, 0, |out| out.u64(movie_duration)));
            }
            for output in outputs {
                mvex.extend(full(b"trex", 0, 0, |out| {
                    out.u32(output.id);
                    out.u32(1);
                    out.u32(0);
                    out.u32(0);
                    out.u32(0);
                }));
            }
        }));
    }));
    out
}

fn trak(output: &Output) -> Vec<u8> {
    let video = output.kind == Kind::Video;
    boxed(b"trak", |trak| {
        trak.extend(full(b"tkhd", 1, 0x3, |out| {
            out.u64(0);
            out.u64(0);
            out.u32(output.id);
            out.u32(0);
            out.u64(0);
            out.zeros(8);
            out.u16(0);
            out.u16(0);
            out.u16(if video { 0 } else { 0x0100 });
            out.u16(0);
            for value in MATRIX {
                out.u32(value);
            }
            let (width, height) = if video { output.display } else { (0, 0) };
            out.u32(width.min(0xFFFF) << 16);
            out.u32(height.min(0xFFFF) << 16);
        }));
        trak.extend(boxed(b"mdia", |mdia| {
            mdia.extend(full(b"mdhd", 1, 0, |out| {
                out.u64(0);
                out.u64(0);
                out.u32(output.timescale);
                out.u64(0);
                out.u16(language_code(output.language.as_deref()));
                out.u16(0);
            }));
            mdia.extend(full(b"hdlr", 0, 0, |out| {
                out.u32(0);
                out.extend_from_slice(if video { b"vide" } else { b"soun" });
                out.zeros(12);
                out.extend_from_slice(if video { b"Video\0" } else { b"Sound\0" });
            }));
            mdia.extend(boxed(b"minf", |minf| {
                if video {
                    minf.extend(full(b"vmhd", 0, 1, |out| out.zeros(8)));
                } else {
                    minf.extend(full(b"smhd", 0, 0, |out| out.zeros(4)));
                }
                minf.extend(boxed(b"dinf", |dinf| {
                    dinf.extend(full(b"dref", 0, 0, |out| {
                        out.u32(1);
                        out.extend(full(b"url ", 0, 1, |_| {}));
                    }));
                }));
                minf.extend(boxed(b"stbl", |stbl| {
                    stbl.extend(full(b"stsd", 0, 0, |out| {
                        out.u32(1);
                        out.extend_from_slice(&output.entry);
                    }));
                    stbl.extend(full(b"stts", 0, 0, |out| out.u32(0)));
                    stbl.extend(full(b"stsc", 0, 0, |out| out.u32(0)));
                    stbl.extend(full(b"stsz", 0, 0, |out| {
                        out.u32(0);
                        out.u32(0);
                    }));
                    stbl.extend(full(b"stco", 0, 0, |out| out.u32(0)));
                }));
            }));
        }));
    })
}

/// One sample of a fragment, in its track's timescale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sample {
    pub decode_time: i64,
    pub duration: u32,
    /// Presentation time minus decode time.
    pub composition_offset: i32,
    pub keyframe: bool,
    pub data: Vec<u8>,
}

/// A media segment: one `moof` with a `traf` per track, then one `mdat`
/// with each track's samples in turn.
pub(crate) fn media_segment(sequence: u32, tracks: &[(&Output, Vec<Sample>)]) -> Vec<u8> {
    let tracks: Vec<_> = tracks
        .iter()
        .filter(|(_, samples)| !samples.is_empty())
        .collect();
    // The moof is written twice: once to learn its size, then with each
    // trun's data offset filled in (from the moof's start to its samples
    // in the mdat).
    let moof = |offsets: &[u32]| {
        boxed(b"moof", |moof| {
            moof.extend(full(b"mfhd", 0, 0, |out| out.u32(sequence)));
            for (index, (output, samples)) in tracks.iter().enumerate() {
                moof.extend(boxed(b"traf", |traf| {
                    // default-base-is-moof
                    traf.extend(full(b"tfhd", 0, 0x02_0000, |out| out.u32(output.id)));
                    let start = samples[0].decode_time.max(0) as u64;
                    traf.extend(full(b"tfdt", 1, 0, |out| out.u64(start)));
                    // data offset, durations, sizes, flags, composition
                    // offsets; version 1 makes the offsets signed.
                    traf.extend(full(b"trun", 1, 0x0F01, |out| {
                        out.u32(samples.len() as u32);
                        out.u32(offsets.get(index).copied().unwrap_or(0));
                        for sample in samples {
                            out.u32(sample.duration);
                            out.u32(sample.data.len() as u32);
                            out.u32(if sample.keyframe {
                                0x0200_0000
                            } else {
                                0x0101_0000
                            });
                            out.extend_from_slice(&sample.composition_offset.to_be_bytes());
                        }
                    }));
                }));
            }
        })
    };
    let size = moof(&[]).len() as u32;
    let mut offsets = Vec::with_capacity(tracks.len());
    let mut at = size + 8;
    for (_, samples) in &tracks {
        offsets.push(at);
        at += samples
            .iter()
            .map(|sample| sample.data.len() as u32)
            .sum::<u32>();
    }
    let mut out = moof(&offsets);
    let mdat_size = at - size;
    out.extend_from_slice(&mdat_size.to_be_bytes());
    out.extend_from_slice(b"mdat");
    for (_, samples) in &tracks {
        for sample in samples {
            out.extend_from_slice(&sample.data);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_strings_follow_rfc_6381() {
        // High profile, level 3.1.
        assert_eq!(
            avc_codec(&[1, 0x64, 0x00, 0x1F, 0xFF]).as_deref(),
            Some("avc1.64001f")
        );
        // Main profile, main tier, level 93 (3.1), progressive source.
        let mut hevc = vec![1u8, 0x01, 0x60, 0, 0, 0, 0x90, 0, 0, 0, 0, 0, 93];
        hevc.extend([0xF0, 0, 0xFC]);
        assert_eq!(hevc_codec(&hevc).as_deref(), Some("hvc1.1.6.L93.90"));
        // Main profile, level 8 (4.0), main tier, 8 bits.
        assert_eq!(
            av1_codec(&[0x81, 0x08, 0x0C, 0]).as_deref(),
            Some("av01.0.08M.08")
        );
    }

    #[test]
    fn opus_head_becomes_big_endian_dops() {
        let mut head = b"OpusHead".to_vec();
        head.extend([1, 2, 0x38, 0x01, 0x80, 0xBB, 0, 0, 0, 0, 0]);
        let dops = dops(&head).unwrap();
        assert_eq!(&dops[4..8], b"dOps");
        assert_eq!(&dops[8..], &[0, 2, 0x01, 0x38, 0, 0, 0xBB, 0x80, 0, 0, 0]);
    }

    #[test]
    fn languages_pack_into_fifteen_bits() {
        // "eng": 5, 14, 7.
        assert_eq!(language_code(Some("en")), (5 << 10) | (14 << 5) | 7);
        assert_eq!(language_code(Some("en-GB")), language_code(Some("eng")));
        assert_eq!(language_code(None), (21 << 10) | (14 << 5) | 4);
    }

    #[test]
    fn data_offsets_point_at_each_tracks_samples() {
        let output = |id, kind| Output {
            id,
            source: u64::from(id),
            kind,
            timescale: 1000,
            entry: Vec::new(),
            codec: String::new(),
            width: 0,
            height: 0,
            display: (0, 0),
            language: None,
            default_duration: None,
        };
        let (video, audio) = (output(1, Kind::Video), output(2, Kind::Audio));
        let sample = |data: &[u8]| Sample {
            decode_time: 0,
            duration: 10,
            composition_offset: 0,
            keyframe: true,
            data: data.to_vec(),
        };
        let segment = media_segment(
            1,
            &[
                (&video, vec![sample(b"VVV")]),
                (&audio, vec![sample(b"AA"), sample(b"aa")]),
            ],
        );
        let moof_size =
            u32::from_be_bytes([segment[0], segment[1], segment[2], segment[3]]) as usize;
        assert_eq!(&segment[moof_size + 4..moof_size + 8], b"mdat");
        assert_eq!(&segment[moof_size + 8..], b"VVVAAaa");
        // Each trun's data offset, read back.
        let offsets: Vec<u32> = segment
            .windows(4)
            .enumerate()
            .filter(|(_, window)| window == b"trun")
            .map(|(at, _)| {
                u32::from_be_bytes([
                    segment[at + 12],
                    segment[at + 13],
                    segment[at + 14],
                    segment[at + 15],
                ])
            })
            .collect();
        assert_eq!(offsets, [moof_size as u32 + 8, moof_size as u32 + 11]);
    }
}
