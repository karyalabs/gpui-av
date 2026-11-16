//! Text tracks inside an MP4 (3GPP timed text, `tx3g`, which ffmpeg calls
//! `mov_text`), read as cues: for players that leave them out (Media
//! Foundation's MP4 source drops them entirely).

use std::io::{self, Read, Seek, SeekFrom};
use std::time::Duration;

use crate::fragments::{box_header, child, children, read_body, u32_at, u64_at, version_flags};

/// The longest `moov` read into memory.
const MAX_MOOV: u64 = 64 * 1024 * 1024;
/// More samples than this in one text track is not a subtitle track.
const MAX_SAMPLES: usize = 1_000_000;

/// A text track of an MP4.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mp4TextTrack {
    /// The track's ID in the file.
    pub id: u32,
    /// Its ISO 639-2 language, when it states one (not `und`).
    pub language: Option<String>,
    /// Its cues in order; an empty sample (a gap) is left out.
    pub cues: Vec<Mp4Cue>,
}

/// One cue of an [`Mp4TextTrack`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mp4Cue {
    /// When the text appears.
    pub start: Duration,
    /// When it goes.
    pub end: Duration,
    /// The text, lines split by `\n`, with styling left out.
    pub text: String,
}

/// The 3GPP timed text tracks of a (not fragmented) MP4, in file order:
/// none for a file without any, or one that is not an MP4. Only the
/// `moov` and the text samples are read.
pub fn mp4_text_tracks<R: Read + Seek>(reader: &mut R) -> io::Result<Vec<Mp4TextTrack>> {
    let Some(moov) = find_moov(reader)? else {
        return Ok(Vec::new());
    };
    let mut tracks = Vec::new();
    for (kind, trak) in children(&moov) {
        if kind != b"trak" {
            continue;
        }
        let Some(table) = text_track(trak) else {
            continue;
        };
        let mut cues = Vec::new();
        for sample in &table.samples {
            if sample.size < 2 {
                continue;
            }
            reader.seek(SeekFrom::Start(sample.offset))?;
            let body = read_body(reader, u64::from(sample.size.min(64 * 1024)))?;
            let length = usize::from(u16::from_be_bytes([body[0], body[1]]));
            let Some(text) = body.get(2..2 + length) else {
                continue;
            };
            let text = decode(text);
            if text.trim().is_empty() {
                continue;
            }
            let at =
                |ticks: u64| Duration::from_secs_f64(ticks as f64 / f64::from(table.timescale));
            cues.push(Mp4Cue {
                start: at(sample.start),
                end: at(sample.start + sample.duration),
                text,
            });
        }
        tracks.push(Mp4TextTrack {
            id: table.id,
            language: table.language,
            cues,
        });
    }
    Ok(tracks)
}

fn find_moov<R: Read + Seek>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
    let file_length = reader.seek(SeekFrom::End(0))?;
    let mut at = 0u64;
    while at + 8 <= file_length {
        reader.seek(SeekFrom::Start(at))?;
        let Some((kind, header, size)) = box_header(reader, file_length - at)? else {
            break;
        };
        if at == 0
            && !matches!(
                &kind,
                b"ftyp" | b"moov" | b"mdat" | b"free" | b"wide" | b"skip"
            )
        {
            return Ok(None);
        }
        if &kind == b"moov" {
            let body = size - header;
            if body > MAX_MOOV {
                return Ok(None);
            }
            return read_body(reader, body).map(Some);
        }
        at += size;
    }
    Ok(None)
}

struct Table {
    id: u32,
    timescale: u32,
    language: Option<String>,
    samples: Vec<Sample>,
}

struct Sample {
    offset: u64,
    size: u32,
    start: u64,
    duration: u64,
}

/// The sample table of a `trak` that holds 3GPP timed text.
fn text_track(trak: &[u8]) -> Option<Table> {
    let tkhd = child(trak, b"tkhd")?;
    let (version, _) = version_flags(tkhd)?;
    let id = u32_at(tkhd, if version == 1 { 20 } else { 12 })?;
    let mdia = child(trak, b"mdia")?;
    let mdhd = child(mdia, b"mdhd")?;
    let (version, _) = version_flags(mdhd)?;
    let (timescale, language_at) = if version == 1 {
        (u32_at(mdhd, 20)?, 32)
    } else {
        (u32_at(mdhd, 12)?, 20)
    };
    if timescale == 0 {
        return None;
    }
    let packed = u16::from_be_bytes(mdhd.get(language_at..language_at + 2)?.try_into().ok()?);
    let language: String = [10u16, 5, 0]
        .iter()
        .map(|shift| char::from(((packed >> shift) & 0x1f) as u8 + 0x60))
        .collect();
    let language = (language.chars().all(|letter| letter.is_ascii_lowercase())
        && language != "und")
        .then_some(language);
    let stbl = child(child(mdia, b"minf")?, b"stbl")?;
    // stsd: version and flags, entry count, then the first entry's box.
    let stsd = child(stbl, b"stsd")?;
    if stsd.get(12..16)? != b"tx3g" {
        return None;
    }
    let durations = entries(child(stbl, b"stts")?, 2)?;
    let chunks = entries(child(stbl, b"stsc")?, 3)?;
    let stsz = child(stbl, b"stsz")?;
    let fixed = u32_at(stsz, 4)?;
    let count = u32_at(stsz, 8)? as usize;
    if count > MAX_SAMPLES {
        return None;
    }
    let sizes: Vec<u32> = if fixed != 0 {
        vec![fixed; count]
    } else {
        (0..count)
            .map(|index| u32_at(stsz, 12 + 4 * index))
            .collect::<Option<_>>()?
    };
    let offsets: Vec<u64> = match child(stbl, b"stco") {
        Some(stco) => entries(stco, 1)?
            .into_iter()
            .map(|entry| u64::from(entry[0]))
            .collect(),
        None => {
            let co64 = child(stbl, b"co64")?;
            let count = u32_at(co64, 4)? as usize;
            (0..count.min(MAX_SAMPLES))
                .map(|index| u64_at(co64, 8 + 8 * index))
                .collect::<Option<_>>()?
        }
    };
    // Each sample's offset: chunks hold runs of samples back to back.
    let mut samples = Vec::with_capacity(count);
    let mut sample = 0usize;
    for (chunk, &chunk_offset) in offsets.iter().enumerate() {
        let chunk_number = chunk as u32 + 1;
        let per_chunk = chunks
            .iter()
            .rev()
            .find(|entry| entry[0] <= chunk_number)
            .map_or(0, |entry| entry[1]);
        let mut offset = chunk_offset;
        for _ in 0..per_chunk {
            let Some(&size) = sizes.get(sample) else {
                break;
            };
            samples.push(Sample {
                offset,
                size,
                start: 0,
                duration: 0,
            });
            offset += u64::from(size);
            sample += 1;
        }
    }
    let mut start = 0u64;
    let mut index = 0usize;
    'durations: for entry in durations {
        for _ in 0..entry[0] {
            let Some(sample) = samples.get_mut(index) else {
                break 'durations;
            };
            sample.start = start;
            sample.duration = u64::from(entry[1]);
            start += u64::from(entry[1]);
            index += 1;
        }
    }
    Some(Table {
        id,
        timescale,
        language,
        samples,
    })
}

/// A full box's entries of `width` 32-bit fields each, after its count.
fn entries(data: &[u8], width: usize) -> Option<Vec<Vec<u32>>> {
    let count = u32_at(data, 4)? as usize;
    if count > MAX_SAMPLES {
        return None;
    }
    (0..count)
        .map(|index| {
            (0..width)
                .map(|field| u32_at(data, 8 + 4 * (index * width + field)))
                .collect()
        })
        .collect()
}

/// A sample's text: UTF-16 after its byte order mark, else UTF-8.
fn decode(text: &[u8]) -> String {
    let text = match text {
        [0xfe, 0xff, rest @ ..] => {
            let units: Vec<u16> = rest
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_be_bytes(*pair))
                .collect();
            String::from_utf16_lossy(&units)
        }
        [0xef, 0xbb, 0xbf, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(text).into_owned(),
    };
    text.replace("\r\n", "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    fn full(kind: &[u8; 4], version: u8, body: &[u8]) -> Vec<u8> {
        let mut data = vec![version, 0, 0, 0];
        data.extend_from_slice(body);
        boxed(kind, &data)
    }

    fn words(values: &[u32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_be_bytes())
            .collect()
    }

    fn sample(text: &[u8]) -> Vec<u8> {
        let mut out = (text.len() as u16).to_be_bytes().to_vec();
        out.extend_from_slice(text);
        out
    }

    /// An MP4 whose one track is 3GPP timed text: "Hello" from 0 to 1 s, a
    /// gap to 2 s, then two lines in UTF-16 to 3.5 s, at a timescale of
    /// 1000, in French.
    fn clip() -> Vec<u8> {
        let samples = [
            sample(b"Hello"),
            sample(b""),
            sample(&[0xfe, 0xff, 0, b'A', 0, b'\n', 0, b'B']),
        ];
        let ftyp = boxed(b"ftyp", b"isom\0\0\0\0isom");
        let mdat_start = ftyp.len() as u32 + 8;
        let mdat = boxed(b"mdat", &samples.concat());
        // "fra", five bits a letter after subtracting 0x60.
        let language =
            ((b'f' - 0x60) as u16) << 10 | ((b'r' - 0x60) as u16) << 5 | (b'a' - 0x60) as u16;
        let mut mdhd = words(&[0, 0, 1000, 3500]);
        mdhd.extend_from_slice(&language.to_be_bytes());
        mdhd.extend_from_slice(&[0, 0]);
        let stsd = full(
            b"stsd",
            0,
            &[words(&[1]), boxed(b"tx3g", &[0; 38])].concat(),
        );
        let stts = full(b"stts", 0, &words(&[2, 2, 1000, 1, 1500]));
        let stsc = full(b"stsc", 0, &words(&[2, 1, 2, 1, 2, 1, 1]));
        let sizes: Vec<u32> = samples.iter().map(|sample| sample.len() as u32).collect();
        let stsz = full(b"stsz", 0, &[words(&[0, 3]), words(&sizes)].concat());
        let second_chunk = mdat_start + sizes[0] + sizes[1];
        let stco = full(b"stco", 0, &words(&[2, mdat_start, second_chunk]));
        let stbl = boxed(b"stbl", &[stsd, stts, stsc, stsz, stco].concat());
        let minf = boxed(b"minf", &stbl);
        let mdia = boxed(b"mdia", &[full(b"mdhd", 0, &mdhd), minf].concat());
        let tkhd = full(b"tkhd", 0, &words(&[0, 0, 7, 0, 3500]));
        let trak = boxed(b"trak", &[tkhd, mdia].concat());
        let moov = boxed(b"moov", &trak);
        [ftyp, mdat, moov].concat()
    }

    #[test]
    fn reads_timed_text_cues() {
        let tracks = mp4_text_tracks(&mut io::Cursor::new(clip())).expect("read");
        let seconds = Duration::from_secs_f64;
        assert_eq!(
            tracks,
            vec![Mp4TextTrack {
                id: 7,
                language: Some("fra".into()),
                cues: vec![
                    Mp4Cue {
                        start: seconds(0.0),
                        end: seconds(1.0),
                        text: "Hello".into(),
                    },
                    Mp4Cue {
                        start: seconds(2.0),
                        end: seconds(3.5),
                        text: "A\nB".into(),
                    },
                ],
            }]
        );
    }

    #[test]
    fn other_files_have_none() {
        for data in [
            Vec::new(),
            b"not an mp4 at all".to_vec(),
            clip()[..40].to_vec(),
        ] {
            let tracks = mp4_text_tracks(&mut io::Cursor::new(data)).unwrap_or_default();
            assert!(tracks.is_empty());
        }
    }

    /// Reads the clips ffmpeg writes with `-c:s mov_text`
    /// (`GPUI_AV_MOV_TEXT`).
    #[test]
    #[ignore = "needs GPUI_AV_MOV_TEXT"]
    fn reads_ffmpeg_clips() {
        let path = std::env::var("GPUI_AV_MOV_TEXT").expect("GPUI_AV_MOV_TEXT");
        let mut file = std::fs::File::open(path).expect("open");
        let tracks = mp4_text_tracks(&mut file).expect("read");
        eprintln!("{tracks:#?}");
        assert!(tracks.iter().any(|track| !track.cues.is_empty()));
    }
}
