//! A small HTTP server on the loopback interface that serves one remuxed
//! file as HLS, for players that take fragmented MP4 only through a
//! playlist (AVFoundation). Not in a browser, which has no sockets; there
//! the segments go to Media Source Extensions instead.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, BufRead, BufReader, Read, Seek, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::{RemuxError, Remuxer};

/// A request that has not sent its headers by then is dropped, so a stuck
/// client cannot hold a thread.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a request waits for a file opened in the background.
const OPEN_TIMEOUT: Duration = Duration::from_secs(60);

/// What the server knows of its file once it is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
    /// The file's length in time.
    pub duration: Duration,
    /// The video's coded size, if there is video.
    pub video_size: Option<(u32, u32)>,
    /// Whether there is sound.
    pub has_audio: bool,
}

/// Why the server could not serve its file: opening it failed, or making
/// a segment did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// What kind of failure.
    pub kind: FailureKind,
    /// What went wrong, for a person.
    pub message: String,
}

/// The kind of a [`Failure`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FailureKind {
    /// Not Matroska or WebM, or something MP4 cannot carry.
    Unsupported,
    /// The file is damaged.
    Damaged,
    /// Reading the file failed, with the reader's error kind (a
    /// [`RangeSource`](crate::RangeSource) reports HTTP statuses as
    /// `NotFound` and `PermissionDenied`).
    Io(io::ErrorKind),
}

impl From<&RemuxError> for Failure {
    fn from(error: &RemuxError) -> Self {
        let kind = match error {
            RemuxError::Invalid(_) => FailureKind::Damaged,
            RemuxError::Io(error) => FailureKind::Io(error.kind()),
            _ => FailureKind::Unsupported,
        };
        Self {
            kind,
            message: error.to_string(),
        }
    }
}

#[derive(Clone, Debug)]
enum Status {
    Opening,
    Ready(Summary),
    Failed(Failure),
}

/// The status, and a signal for requests waiting on it.
#[derive(Default)]
struct Watched {
    status: Mutex<Option<Status>>,
    changed: Condvar,
}

impl Watched {
    fn set(&self, status: Status) {
        if let Ok(mut current) = self.status.lock() {
            // A failure stays: a later request must not hide it.
            if !matches!(*current, Some(Status::Failed(_))) {
                *current = Some(status);
            }
        }
        self.changed.notify_all();
    }

    fn get(&self) -> Option<Status> {
        self.status.lock().ok().and_then(|status| status.clone())
    }

    /// Waits while the file opens.
    fn wait(&self) -> Option<Status> {
        let status = self.status.lock().ok()?;
        let (status, _) = self
            .changed
            .wait_timeout_while(status, OPEN_TIMEOUT, |status| {
                matches!(status, Some(Status::Opening) | None)
            })
            .ok()?;
        status.clone()
    }
}

/// Serves a [`Remuxer`]'s playlist, init segment and media segments on
/// `127.0.0.1` until dropped.
///
/// The URL holds a random token, so other programs on the machine cannot
/// guess it. Requests outside it get 404.
pub struct HlsServer {
    url: String,
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
    watched: Arc<Watched>,
}

struct Shared<R> {
    parts: Mutex<Option<Parts<R>>>,
    watched: Arc<Watched>,
    prefix: String,
    /// Set when the server is dropped: open connections close after their
    /// current request, so the file is not held open.
    stop: Arc<AtomicBool>,
}

/// What is served, once the file is open.
struct Parts<R> {
    remuxer: Remuxer<R>,
    playlist: String,
    init: Vec<u8>,
    media_type: &'static str,
}

impl<R: Read + Seek> Parts<R> {
    fn new(remuxer: Remuxer<R>) -> (Self, Summary) {
        let summary = Summary {
            duration: remuxer.duration(),
            video_size: remuxer.video_size(),
            has_audio: remuxer.has_audio(),
        };
        let parts = Self {
            playlist: remuxer.hls_playlist("init.mp4", |index| format!("{index}.m4s")),
            init: remuxer.init_segment(),
            media_type: if summary.video_size.is_some() {
                "video/mp4"
            } else {
                "audio/mp4"
            },
            remuxer,
        };
        (parts, summary)
    }
}

impl HlsServer {
    /// Starts serving an open file on a free port of the loopback
    /// interface.
    pub fn start<R: Read + Seek + Send + 'static>(remuxer: Remuxer<R>) -> io::Result<Self> {
        let (parts, summary) = Parts::new(remuxer);
        let (server, shared) = Self::listen(Some(parts))?;
        shared.watched.set(Status::Ready(summary));
        Ok(server)
    }

    /// Starts serving at once and opens the file on a background thread
    /// with `open`, for a file whose reads block (a URL). Requests wait
    /// for it; a failure is kept for [`HlsServer::failure`].
    pub fn open_in_background<R, F>(open: F) -> io::Result<Self>
    where
        R: Read + Seek + Send + 'static,
        F: FnOnce() -> Result<Remuxer<R>, RemuxError> + Send + 'static,
    {
        let (server, shared) = Self::listen::<R>(None)?;
        shared.watched.set(Status::Opening);
        thread::Builder::new()
            .name("gpui-av-hls-open".into())
            .spawn(move || match open() {
                Ok(remuxer) => {
                    let (parts, summary) = Parts::new(remuxer);
                    if let Ok(mut slot) = shared.parts.lock() {
                        *slot = Some(parts);
                    }
                    shared.watched.set(Status::Ready(summary));
                }
                Err(error) => shared.watched.set(Status::Failed(Failure::from(&error))),
            })?;
        Ok(server)
    }

    /// What the file holds, once it is open.
    pub fn summary(&self) -> Option<Summary> {
        match self.watched.get()? {
            Status::Ready(summary) => Some(summary),
            _ => None,
        }
    }

    /// Why the file could not be served, if it could not: opening it
    /// failed, or a segment did.
    pub fn failure(&self) -> Option<Failure> {
        match self.watched.get()? {
            Status::Failed(failure) => Some(failure),
            _ => None,
        }
    }

    fn listen<R: Read + Seek + Send + 'static>(
        parts: Option<Parts<R>>,
    ) -> io::Result<(Self, Arc<Shared<R>>)> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        let prefix = format!("/{}", token());
        let stop = Arc::new(AtomicBool::new(false));
        let watched = Arc::new(Watched::default());
        let shared = Arc::new(Shared {
            parts: Mutex::new(parts),
            watched: watched.clone(),
            prefix: prefix.clone(),
            stop: stop.clone(),
        });
        let accept = thread::Builder::new().name("gpui-av-hls".into()).spawn({
            let stop = stop.clone();
            let shared = shared.clone();
            move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    let shared = shared.clone();
                    let spawned = thread::Builder::new()
                        .name("gpui-av-hls-request".into())
                        .spawn(move || serve(stream, &shared));
                    if spawned.is_err() {
                        continue;
                    }
                }
            }
        })?;
        let server = Self {
            url: format!("http://{address}{prefix}/index.m3u8"),
            address,
            stop,
            accept: Some(accept),
            watched,
        };
        Ok((server, shared))
    }

    /// The playlist's URL.
    pub fn url(&self) -> &str {
        &self.url
    }
}

impl Drop for HlsServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accepting thread so it sees the flag. A failed connection
        // means the listener is gone already, and the join returns at once.
        if let Err(error) = TcpStream::connect(self.address) {
            log_failure("waking the HLS server to stop", &error);
        }
        if let Some(accept) = self.accept.take()
            && accept.join().is_err()
        {
            log_failure("stopping the HLS server", &"its thread panicked");
        }
    }
}

fn log_failure(what: &str, error: &dyn std::fmt::Display) {
    eprintln!("gpui-av-remux: {what} failed: {error}");
}

/// 128 random bits from the standard library's hasher keys, as hex.
fn token() -> String {
    let half = || RandomState::new().build_hasher().finish();
    format!("{:016x}{:016x}", half(), half())
}

/// Answers requests on one connection until the client closes it.
fn serve<R: Read + Seek>(stream: TcpStream, shared: &Shared<R>) {
    if stream.set_read_timeout(Some(READ_TIMEOUT)).is_err() {
        return;
    }
    let Ok(writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let mut writer = io::BufWriter::new(writer);
    while let Some(request) = read_request(&mut reader) {
        let keep_alive = request.keep_alive;
        if respond(&mut writer, &request, shared).is_err()
            || !keep_alive
            || shared.stop.load(Ordering::SeqCst)
        {
            break;
        }
    }
    if let Ok(stream) = writer.into_inner() {
        // The client may have closed it first.
        stream.shutdown(Shutdown::Both).ok();
    }
}

struct Request {
    method: String,
    path: String,
    range: Option<(u64, Option<u64>)>,
    keep_alive: bool,
}

fn read_request(reader: &mut impl BufRead) -> Option<Request> {
    let mut line = String::new();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let version = parts.next().unwrap_or("HTTP/1.0");
    let mut keep_alive = version == "HTTP/1.1";
    let mut range = None;
    loop {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("connection") {
            keep_alive = !value.eq_ignore_ascii_case("close");
        } else if name.eq_ignore_ascii_case("range") {
            range = parse_range(value);
        }
    }
    Some(Request {
        method,
        path,
        range,
        keep_alive,
    })
}

/// `bytes=a-b` or `bytes=a-`; other forms are served whole.
fn parse_range(value: &str) -> Option<(u64, Option<u64>)> {
    let (start, end) = value.strip_prefix("bytes=")?.split_once('-')?;
    let start = start.trim().parse().ok()?;
    let end = match end.trim() {
        "" => None,
        end => Some(end.parse().ok()?),
    };
    Some((start, end))
}

fn respond<R: Read + Seek>(
    writer: &mut impl Write,
    request: &Request,
    shared: &Shared<R>,
) -> io::Result<()> {
    if request.method != "GET" && request.method != "HEAD" {
        return status(writer, "405 Method Not Allowed");
    }
    let Some(name) = request
        .path
        .strip_prefix(shared.prefix.as_str())
        .and_then(|rest| rest.strip_prefix('/'))
    else {
        return status(writer, "404 Not Found");
    };
    match shared.watched.wait() {
        Some(Status::Ready(_)) => {}
        Some(Status::Failed(_)) => return status(writer, "502 Bad Gateway"),
        _ => return status(writer, "504 Gateway Timeout"),
    }
    let Ok(mut parts) = shared.parts.lock() else {
        return status(writer, "500 Internal Server Error");
    };
    let Some(parts) = parts.as_mut() else {
        return status(writer, "500 Internal Server Error");
    };
    let segment;
    let (body, content_type): (&[u8], &str) = match name {
        "index.m3u8" => (parts.playlist.as_bytes(), "application/vnd.apple.mpegurl"),
        "init.mp4" => (&parts.init, parts.media_type),
        _ => {
            let Some(index) = name
                .strip_suffix(".m4s")
                .and_then(|index| index.parse::<usize>().ok())
            else {
                return status(writer, "404 Not Found");
            };
            if index >= parts.remuxer.segments().len() {
                return status(writer, "404 Not Found");
            }
            match parts.remuxer.media_segment(index) {
                Ok(bytes) => segment = bytes,
                Err(error) => {
                    shared.watched.set(Status::Failed(Failure::from(&error)));
                    return status(writer, "502 Bad Gateway");
                }
            }
            (&segment, "video/iso.segment")
        }
    };
    let total = body.len() as u64;
    let body = match request.range {
        Some((start, end)) if start < total => {
            let end = end.unwrap_or(total - 1).min(total - 1);
            if end < start {
                return status(writer, "416 Range Not Satisfiable");
            }
            write!(
                writer,
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{total}\r\n"
            )?;
            &body[start as usize..=end as usize]
        }
        Some(_) => return status(writer, "416 Range Not Satisfiable"),
        None => {
            write!(writer, "HTTP/1.1 200 OK\r\n")?;
            body
        }
    };
    write!(
        writer,
        "Content-Type: {content_type}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\n\
         Cache-Control: no-store\r\n\r\n",
        body.len()
    )?;
    if request.method == "GET" {
        writer.write_all(body)?;
    }
    writer.flush()
}

fn status(writer: &mut impl Write, status: &str) -> io::Result<()> {
    write!(writer, "HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n")?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::parse_range;

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-99"), Some((0, Some(99))));
        assert_eq!(parse_range("bytes=100-"), Some((100, None)));
        assert_eq!(parse_range("bytes=-500"), None);
        assert_eq!(parse_range("items=0-1"), None);
    }
}
