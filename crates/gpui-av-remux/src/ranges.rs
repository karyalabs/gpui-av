//! Reading a file over the network with range requests, for remuxing a
//! Matroska or WebM URL where reads may block (a server thread). The
//! requests themselves are the platform's: see [`RangeSource`].

use std::collections::VecDeque;
use std::io::{self, Read, Seek, SeekFrom};

/// The first request's size. Reads that carry on from the last request
/// double it, up to [`MAX_CHUNK`]; a jump elsewhere starts small again.
const MIN_CHUNK: u64 = 256 * 1024;
const MAX_CHUNK: u64 = 8 * 1024 * 1024;
/// How many fetched runs are kept: enough for a segment's clusters and the
/// header's cues, a few tens of megabytes at most.
const KEEP: usize = 8;

/// Bytes a server sent for a range request.
pub struct Fetched {
    /// The bytes, from the start asked for. Fewer than asked for means the
    /// file ends there.
    pub bytes: Vec<u8>,
    /// The file's whole length, where the server said (`Content-Range`).
    pub length: Option<u64>,
}

/// Something that answers range requests: `fetch` returns up to `length`
/// bytes from `start`. It blocks, so a [`RangeReader`] over it is for a
/// background thread.
pub trait RangeSource: Send {
    /// Fetches up to `length` bytes from `start`.
    fn fetch(&mut self, start: u64, length: u64) -> io::Result<Fetched>;
}

/// A [`Read`] and [`Seek`] over a [`RangeSource`], fetching in growing
/// chunks and keeping the last few.
pub struct RangeReader<S> {
    source: S,
    position: u64,
    length: Option<u64>,
    runs: VecDeque<(u64, Vec<u8>)>,
    chunk: u64,
    /// Where the last fetch ended, to tell a read that carries on.
    last_end: Option<u64>,
}

impl<S: RangeSource> RangeReader<S> {
    /// A reader at the start of the file. Nothing is fetched until the
    /// first read.
    pub fn new(source: S) -> Self {
        Self {
            source,
            position: 0,
            length: None,
            runs: VecDeque::new(),
            chunk: MIN_CHUNK,
            last_end: None,
        }
    }

    fn run_at(&self, at: u64) -> Option<(u64, &[u8])> {
        self.runs
            .iter()
            .find(|(start, run)| *start <= at && at < start + run.len() as u64)
            .map(|(start, run)| (*start, run.as_slice()))
    }

    fn fetch_at(&mut self, at: u64) -> io::Result<()> {
        self.chunk = if self.last_end == Some(at) {
            (self.chunk * 2).min(MAX_CHUNK)
        } else {
            MIN_CHUNK
        };
        let fetched = self.source.fetch(at, self.chunk)?;
        if let Some(length) = fetched.length {
            self.length = Some(length);
        }
        let got = fetched.bytes.len() as u64;
        if got < self.chunk && self.length.is_none() {
            self.length = Some(at + got);
        }
        self.last_end = Some(at + got);
        if got > 0 {
            if self.runs.len() >= KEEP {
                self.runs.pop_front();
            }
            self.runs.push_back((at, fetched.bytes));
        }
        Ok(())
    }
}

impl<S: RangeSource> Read for RangeReader<S> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() || self.length.is_some_and(|length| self.position >= length) {
            return Ok(0);
        }
        if self.run_at(self.position).is_none() {
            self.fetch_at(self.position)?;
        }
        let Some((start, run)) = self.run_at(self.position) else {
            // The server sent nothing: the end of the file.
            return Ok(0);
        };
        let from = (self.position - start) as usize;
        let count = buffer.len().min(run.len() - from);
        buffer[..count].copy_from_slice(&run[from..from + count]);
        self.position += count as u64;
        Ok(count)
    }
}

impl<S: RangeSource> Seek for RangeReader<S> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let position = match to {
            SeekFrom::Start(position) => Some(position),
            SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
            SeekFrom::End(offset) => {
                if self.length.is_none() {
                    self.fetch_at(0)?;
                }
                let length = self.length.ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::Unsupported,
                        "the server did not say the length",
                    )
                })?;
                length.checked_add_signed(offset)
            }
        };
        self.position = position
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before the start"))?;
        Ok(self.position)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Read, Seek, SeekFrom};

    use super::{Fetched, RangeReader, RangeSource};

    struct Memory {
        bytes: Vec<u8>,
        requests: Vec<(u64, u64)>,
    }

    impl RangeSource for Memory {
        fn fetch(&mut self, start: u64, length: u64) -> io::Result<Fetched> {
            self.requests.push((start, length));
            let from = (start as usize).min(self.bytes.len());
            let to = (from + length as usize).min(self.bytes.len());
            Ok(Fetched {
                bytes: self.bytes[from..to].to_vec(),
                length: Some(self.bytes.len() as u64),
            })
        }
    }

    #[test]
    fn reads_like_the_file_and_grows_sequential_requests() {
        let bytes: Vec<u8> = (0..3_000_000u32).map(|index| index as u8).collect();
        let mut reader = RangeReader::new(Memory {
            bytes: bytes.clone(),
            requests: Vec::new(),
        });
        let mut all = Vec::new();
        reader.read_to_end(&mut all).expect("read");
        assert_eq!(all, bytes);
        let sizes: Vec<u64> = reader
            .source
            .requests
            .iter()
            .map(|(_, size)| *size)
            .collect();
        assert_eq!(sizes[..3], [256 * 1024, 512 * 1024, 1024 * 1024]);
        reader.seek(SeekFrom::End(-10)).expect("seek from the end");
        let mut tail = Vec::new();
        reader.read_to_end(&mut tail).expect("tail");
        assert_eq!(tail, bytes[bytes.len() - 10..]);
    }
}
