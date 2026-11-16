//! The length of a fragmented MP4 whose header does not state it: an empty
//! `moov` (as live recorders and some cameras write) leaves the length to
//! the fragments, and players that read only the header report none.

use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom};
use std::time::Duration;

/// A box larger than this is not read into memory: `moov` and `moof` are
/// small next to the media.
const MAX_HEADER_BOX: u64 = 64 * 1024 * 1024;

/// The length of a fragmented MP4, from the end of its last fragment in each
/// track: `None` for a file with no fragments, or one whose header already
/// states a length (`mvhd` or `mehd`). Only the boxes' headers, the `moov`
/// and the `moof`s are read; the media is skipped.
pub fn fragmented_mp4_length<R: Read + Seek>(reader: &mut R) -> io::Result<Option<Duration>> {
    let file_length = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(0))?;
    let mut tracks: HashMap<u32, Track> = HashMap::new();
    let mut moov_seen = false;
    let mut at = 0u64;
    while at + 8 <= file_length {
        reader.seek(SeekFrom::Start(at))?;
        let Some((kind, header, size)) = box_header(reader, file_length - at)? else {
            break;
        };
        let body = size - header;
        match &kind {
            b"moov" if body <= MAX_HEADER_BOX => {
                let moov = read_body(reader, body)?;
                match read_moov(&moov) {
                    Some(Moov::Stated) => return Ok(None),
                    Some(Moov::Tracks(found)) => tracks = found,
                    None => return Ok(None),
                }
                moov_seen = true;
            }
            b"moof" if moov_seen && body <= MAX_HEADER_BOX => {
                let moof = read_body(reader, body)?;
                read_moof(&moof, &mut tracks);
            }
            _ => {}
        }
        at += size;
    }
    let longest = tracks
        .values()
        .filter(|track| track.timescale > 0 && track.end > 0)
        .map(|track| Duration::from_secs_f64(track.end as f64 / f64::from(track.timescale)))
        .max();
    Ok(longest)
}

#[derive(Debug, Default)]
struct Track {
    timescale: u32,
    default_duration: u32,
    /// Where the latest fragment ends, in the track's timescale.
    end: u64,
}

enum Moov {
    /// The header states a length; nothing to work out.
    Stated,
    Tracks(HashMap<u32, Track>),
}

/// A box's type, header size and whole size, or `None` past the end.
pub(crate) fn box_header<R: Read>(
    reader: &mut R,
    left: u64,
) -> io::Result<Option<([u8; 4], u64, u64)>> {
    let mut start = [0u8; 8];
    reader.read_exact(&mut start)?;
    let kind = [start[4], start[5], start[6], start[7]];
    let (header, size) = match u32::from_be_bytes([start[0], start[1], start[2], start[3]]) {
        // To the end of the file.
        0 => (8, left),
        1 => {
            let mut large = [0u8; 8];
            reader.read_exact(&mut large)?;
            (16, u64::from_be_bytes(large))
        }
        size => (8, u64::from(size)),
    };
    if size < header || size > left {
        return Ok(None);
    }
    Ok(Some((kind, header, size)))
}

pub(crate) fn read_body<R: Read>(reader: &mut R, length: u64) -> io::Result<Vec<u8>> {
    let mut body = vec![0u8; length as usize];
    reader.read_exact(&mut body)?;
    Ok(body)
}

/// The child boxes of `data`, as (type, body).
pub(crate) fn children(data: &[u8]) -> impl Iterator<Item = (&[u8], &[u8])> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        let start = data.get(at..at + 8)?;
        let size = u32::from_be_bytes(start[..4].try_into().ok()?) as usize;
        let (header, size) = match size {
            0 => (8, data.len() - at),
            1 => {
                let large = data.get(at + 8..at + 16)?;
                (16, u64::from_be_bytes(large.try_into().ok()?) as usize)
            }
            size => (8, size),
        };
        if size < header {
            return None;
        }
        let body = data.get(at + header..at + size)?;
        let kind = &start[4..8];
        at += size;
        Some((kind, body))
    })
}

pub(crate) fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    children(data)
        .find(|(found, _)| found == kind)
        .map(|(_, body)| body)
}

pub(crate) fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

pub(crate) fn u64_at(data: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(data.get(at..at + 8)?.try_into().ok()?))
}

/// A full box's version and flags.
pub(crate) fn version_flags(data: &[u8]) -> Option<(u8, u32)> {
    let word = u32_at(data, 0)?;
    Some(((word >> 24) as u8, word & 0x00FF_FFFF))
}

fn read_moov(moov: &[u8]) -> Option<Moov> {
    // mvhd: version, flags, created, modified, timescale, duration.
    let mvhd = child(moov, b"mvhd")?;
    let (version, _) = version_flags(mvhd)?;
    let stated = if version == 1 {
        u64_at(mvhd, 24)?
    } else {
        u64::from(u32_at(mvhd, 16)?)
    };
    let mvex = child(moov, b"mvex");
    let fragment_stated = mvex.and_then(|mvex| child(mvex, b"mehd")).and_then(|mehd| {
        let (version, _) = version_flags(mehd)?;
        if version == 1 {
            u64_at(mehd, 4)
        } else {
            u32_at(mehd, 4).map(u64::from)
        }
    });
    // An all-ones duration means "unknown".
    let known = |value: u64| value != 0 && value != u64::from(u32::MAX) && value != u64::MAX;
    if known(stated) || fragment_stated.is_some_and(known) {
        return Some(Moov::Stated);
    }
    let mut tracks = HashMap::new();
    for (kind, trak) in children(moov) {
        if kind != b"trak" {
            continue;
        }
        let Some(tkhd) = child(trak, b"tkhd") else {
            continue;
        };
        let Some((version, _)) = version_flags(tkhd) else {
            continue;
        };
        let id_at = if version == 1 { 20 } else { 12 };
        let Some(id) = u32_at(tkhd, id_at) else {
            continue;
        };
        let timescale = child(trak, b"mdia")
            .and_then(|mdia| child(mdia, b"mdhd"))
            .and_then(|mdhd| {
                let (version, _) = version_flags(mdhd)?;
                u32_at(mdhd, if version == 1 { 20 } else { 12 })
            })
            .unwrap_or(0);
        tracks.insert(
            id,
            Track {
                timescale,
                ..Track::default()
            },
        );
    }
    for (kind, trex) in mvex.into_iter().flat_map(children) {
        if kind != b"trex" {
            continue;
        }
        // trex: version and flags, track id, sample description index,
        // default duration.
        if let (Some(id), Some(duration)) = (u32_at(trex, 4), u32_at(trex, 12))
            && let Some(track) = tracks.get_mut(&id)
        {
            track.default_duration = duration;
        }
    }
    Some(Moov::Tracks(tracks))
}

fn read_moof(moof: &[u8], tracks: &mut HashMap<u32, Track>) {
    for (kind, traf) in children(moof) {
        if kind != b"traf" {
            continue;
        }
        let Some(tfhd) = child(traf, b"tfhd") else {
            continue;
        };
        let Some(((_, flags), id)) = version_flags(tfhd).zip(u32_at(tfhd, 4)) else {
            continue;
        };
        let Some(track) = tracks.get_mut(&id) else {
            continue;
        };
        // tfhd's optional fields, in order: base data offset (8 bytes),
        // sample description index, default duration.
        let mut at = 8;
        if flags & 0x01 != 0 {
            at += 8;
        }
        if flags & 0x02 != 0 {
            at += 4;
        }
        let default_duration = if flags & 0x08 != 0 {
            u32_at(tfhd, at).unwrap_or(track.default_duration)
        } else {
            track.default_duration
        };
        let start = child(traf, b"tfdt").and_then(|tfdt| {
            let (version, _) = version_flags(tfdt)?;
            if version == 1 {
                u64_at(tfdt, 4)
            } else {
                u32_at(tfdt, 4).map(u64::from)
            }
        });
        // Without tfdt, a fragment follows on from the last.
        let mut end = start.unwrap_or(track.end);
        for (kind, trun) in children(traf) {
            if kind != b"trun" {
                continue;
            }
            let Some(((_, flags), count)) = version_flags(trun).zip(u32_at(trun, 4)) else {
                continue;
            };
            let mut at = 8;
            if flags & 0x01 != 0 {
                at += 4;
            }
            if flags & 0x04 != 0 {
                at += 4;
            }
            let per_sample = [0x100, 0x200, 0x400, 0x800]
                .iter()
                .filter(|flag| flags & **flag != 0)
                .count()
                * 4;
            if flags & 0x100 == 0 {
                end += u64::from(count) * u64::from(default_duration);
                continue;
            }
            for sample in 0..count as usize {
                match u32_at(trun, at + sample * per_sample) {
                    Some(duration) => end += u64::from(duration),
                    None => break,
                }
            }
        }
        track.end = track.end.max(end);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    fn full(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
        let mut inner = ((u32::from(version) << 24) | flags).to_be_bytes().to_vec();
        inner.extend_from_slice(body);
        boxed(kind, &inner)
    }

    fn words(values: &[u32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_be_bytes())
            .collect()
    }

    /// An empty moov with one track (id 1, timescale 90000, default sample
    /// duration 3000), with `mvhd_duration` stated.
    fn moov(mvhd_duration: u32) -> Vec<u8> {
        let mvhd = full(b"mvhd", 0, 0, &words(&[0, 0, 1000, mvhd_duration]));
        let tkhd = full(b"tkhd", 0, 3, &words(&[0, 0, 1, 0, 0]));
        let mdhd = full(b"mdhd", 0, 0, &words(&[0, 0, 90_000, 0]));
        let trak = boxed(b"trak", &[tkhd, boxed(b"mdia", &mdhd)].concat());
        let trex = full(b"trex", 0, 0, &words(&[1, 1, 3000, 0, 0]));
        boxed(b"moov", &[mvhd, trak, boxed(b"mvex", &trex)].concat())
    }

    fn moof(start: u64, durations: Option<&[u32]>, count: u32) -> Vec<u8> {
        let tfhd = full(b"tfhd", 0, 0x02_0000, &words(&[1]));
        let tfdt = full(b"tfdt", 1, 0, &start.to_be_bytes());
        let trun = match durations {
            Some(durations) => full(
                b"trun",
                0,
                0x101,
                &[words(&[durations.len() as u32, 0]), words(durations)].concat(),
            ),
            None => full(b"trun", 0, 0x001, &words(&[count, 0])),
        };
        boxed(b"moof", &boxed(b"traf", &[tfhd, tfdt, trun].concat()))
    }

    fn length(file: &[u8]) -> Option<Duration> {
        fragmented_mp4_length(&mut Cursor::new(file)).expect("read")
    }

    #[test]
    fn adds_up_the_fragments() {
        let file = [
            boxed(b"ftyp", b"isom"),
            moov(0),
            moof(0, Some(&[3000; 30]), 0),
            boxed(b"mdat", &[0; 100]),
            // Default durations, from trex.
            moof(90_000, None, 30),
            boxed(b"mdat", &[0; 100]),
        ]
        .concat();
        assert_eq!(length(&file), Some(Duration::from_secs(2)));
    }

    #[test]
    fn a_stated_length_is_left_alone() {
        let file = [moov(5000), moof(0, Some(&[3000; 30]), 0)].concat();
        assert_eq!(length(&file), None);
    }

    #[test]
    fn a_file_without_fragments_has_none() {
        let file = [boxed(b"ftyp", b"isom"), moov(0), boxed(b"mdat", &[0; 10])].concat();
        assert_eq!(length(&file), None);
        assert_eq!(length(b"not an mp4 at all"), None);
    }

    #[test]
    fn a_cut_file_counts_what_is_there() {
        let mut file = [moov(0), moof(0, Some(&[3000; 30]), 0)].concat();
        file.extend_from_slice(&boxed(b"mdat", &[0; 100])[..50]);
        assert_eq!(length(&file), Some(Duration::from_secs(1)));
    }
}
