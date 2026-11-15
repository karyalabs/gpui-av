//! A file known only in parts, for remuxing what is fetched over the
//! network piece by piece (in a browser, where reads cannot block).
//!
//! The remuxer reads from a [`SparseReader`]. A read of bytes not fetched
//! yet fails, [`RemuxError::missing`] says where, and the caller fetches
//! them, [`SparseFile::insert`]s them and tries again.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read, Seek, SeekFrom};
use std::ops::Range;
use std::rc::Rc;

use crate::RemuxError;

/// The parts of a file fetched so far. Cloning shares them.
#[derive(Clone, Default)]
pub struct SparseFile {
    inner: Rc<RefCell<Parts>>,
}

#[derive(Default)]
struct Parts {
    /// The file's length, once known.
    length: Option<u64>,
    /// Fetched runs of bytes by where they start. Runs do not overlap.
    runs: BTreeMap<u64, Vec<u8>>,
}

/// The error a [`SparseReader`] reads fail with: these bytes are not here.
#[derive(Debug)]
struct Missing(u64);

impl fmt::Display for Missing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "byte {} has not been fetched", self.0)
    }
}

impl std::error::Error for Missing {}

impl RemuxError {
    /// For an error from reading a [`SparseReader`]: the first byte that
    /// was wanted and has not been fetched.
    pub fn missing(&self) -> Option<u64> {
        match self {
            Self::Io(error) => error
                .get_ref()
                .and_then(|inner| inner.downcast_ref::<Missing>())
                .map(|missing| missing.0),
            _ => None,
        }
    }
}

impl SparseFile {
    /// Nothing fetched yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The file's length, once a server has said.
    pub fn length(&self) -> Option<u64> {
        self.inner.borrow().length
    }

    /// Notes the file's length.
    pub fn set_length(&self, length: u64) {
        self.inner.borrow_mut().length = Some(length);
    }

    /// Adds fetched bytes starting at `start`. Bytes already held are kept
    /// as they are.
    pub fn insert(&self, start: u64, bytes: Vec<u8>) {
        let mut parts = self.inner.borrow_mut();
        let end = start + bytes.len() as u64;
        // Keep only the gaps of `start..end` that no run covers.
        let mut gaps: Vec<Range<u64>> = std::iter::once(start..end).collect();
        let touching: Vec<Range<u64>> = parts
            .runs
            .range(..end)
            .map(|(at, run)| *at..*at + run.len() as u64)
            .filter(|run| run.end > start)
            .collect();
        for run in touching {
            gaps = gaps
                .into_iter()
                .flat_map(|gap| {
                    [
                        gap.start..gap.end.min(run.start),
                        gap.start.max(run.end)..gap.end,
                    ]
                })
                .filter(|gap| gap.start < gap.end)
                .collect();
        }
        for gap in gaps {
            let from = (gap.start - start) as usize;
            let to = (gap.end - start) as usize;
            parts.runs.insert(gap.start, bytes[from..to].to_vec());
        }
    }

    /// Whether every byte of `range` is here.
    pub fn has(&self, range: Range<u64>) -> bool {
        self.first_missing(range).is_none()
    }

    /// The first byte of `range` not fetched yet.
    pub fn first_missing(&self, range: Range<u64>) -> Option<u64> {
        let parts = self.inner.borrow();
        let mut at = range.start;
        while at < range.end {
            match parts.run_at(at) {
                Some((start, run)) => at = start + run.len() as u64,
                None => return Some(at),
            }
        }
        None
    }

    /// Drops the bytes inside `range`, to bound memory once a part has been
    /// used. Runs that reach outside it are kept whole.
    pub fn forget(&self, range: Range<u64>) {
        let mut parts = self.inner.borrow_mut();
        let inside: Vec<u64> = parts
            .runs
            .range(range.start..range.end)
            .filter(|(at, run)| **at + run.len() as u64 <= range.end)
            .map(|(at, _)| *at)
            .collect();
        for at in inside {
            parts.runs.remove(&at);
        }
    }

    /// A reader at the start of the file.
    pub fn reader(&self) -> SparseReader {
        SparseReader {
            file: self.clone(),
            position: 0,
        }
    }
}

impl Parts {
    /// The run holding byte `at`, with where it starts.
    fn run_at(&self, at: u64) -> Option<(u64, &Vec<u8>)> {
        let (start, run) = self.runs.range(..=at).next_back()?;
        (at < start + run.len() as u64).then_some((*start, run))
    }
}

/// Reads a [`SparseFile`]. A read of bytes not fetched fails; see
/// [`RemuxError::missing`].
pub struct SparseReader {
    file: SparseFile,
    position: u64,
}

impl Read for SparseReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let parts = self.file.inner.borrow();
        if parts.length.is_some_and(|length| self.position >= length) {
            return Ok(0);
        }
        let Some((start, run)) = parts.run_at(self.position) else {
            return Err(io::Error::other(Missing(self.position)));
        };
        let from = (self.position - start) as usize;
        let count = buffer.len().min(run.len() - from);
        buffer[..count].copy_from_slice(&run[from..from + count]);
        self.position += count as u64;
        Ok(count)
    }
}

impl Seek for SparseReader {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let position = match to {
            SeekFrom::Start(position) => Some(position),
            SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
            SeekFrom::End(offset) => {
                let length = self.file.length().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::Unsupported, "the length is not known yet")
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
    use std::io::{Read, Seek, SeekFrom};

    use super::SparseFile;
    use crate::RemuxError;

    #[test]
    fn reads_what_is_there_and_says_what_is_not() {
        let file = SparseFile::new();
        file.set_length(10);
        file.insert(0, vec![0, 1, 2, 3]);
        file.insert(6, vec![6, 7, 8, 9]);
        let mut reader = file.reader();
        let mut buffer = [0; 4];
        reader.read_exact(&mut buffer).expect("first run");
        assert_eq!(buffer, [0, 1, 2, 3]);
        let error = RemuxError::from(reader.read_exact(&mut buffer).expect_err("a gap"));
        assert_eq!(error.missing(), Some(4));
        assert_eq!(file.first_missing(0..10), Some(4));
        // Overlapping inserts keep the old bytes and fill the gap.
        file.insert(2, vec![92, 93, 4, 5, 96]);
        assert!(file.has(0..10));
        reader.seek(SeekFrom::Start(2)).expect("seek");
        let mut all = Vec::new();
        reader.read_to_end(&mut all).expect("to the end");
        assert_eq!(all, [2, 3, 4, 5, 6, 7, 8, 9]);
        file.forget(4..6);
        assert_eq!(file.first_missing(0..10), Some(4));
    }
}
