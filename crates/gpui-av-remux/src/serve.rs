//! A small HTTP server on the loopback interface that serves one remuxed
//! file as HLS, for players that take fragmented MP4 only through a
//! playlist (AVFoundation). Not in a browser, which has no sockets; there
//! the segments go to Media Source Extensions instead.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, BufRead, BufReader, Read, Seek, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::Remuxer;

/// A request that has not sent its headers by then is dropped, so a stuck
/// client cannot hold a thread.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

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
}

struct Shared<R> {
    remuxer: Mutex<Remuxer<R>>,
    playlist: String,
    init: Vec<u8>,
    prefix: String,
    media_type: &'static str,
    /// Set when the server is dropped: open connections close after their
    /// current request, so the file is not held open.
    stop: Arc<AtomicBool>,
}

impl HlsServer {
    /// Starts serving on a free port of the loopback interface.
    pub fn start<R: Read + Seek + Send + 'static>(remuxer: Remuxer<R>) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        let prefix = format!("/{}", token());
        let media_type = if remuxer.video_size().is_some() {
            "video/mp4"
        } else {
            "audio/mp4"
        };
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared {
            playlist: remuxer.hls_playlist("init.mp4", |index| format!("{index}.m4s")),
            init: remuxer.init_segment(),
            remuxer: Mutex::new(remuxer),
            prefix: prefix.clone(),
            media_type,
            stop: stop.clone(),
        });
        let accept = thread::Builder::new().name("gpui-av-hls".into()).spawn({
            let stop = stop.clone();
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
        Ok(Self {
            url: format!("http://{address}{prefix}/index.m3u8"),
            address,
            stop,
            accept: Some(accept),
        })
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
    let segment;
    let (body, content_type): (&[u8], &str) = match name {
        "index.m3u8" => (shared.playlist.as_bytes(), "application/vnd.apple.mpegurl"),
        "init.mp4" => (&shared.init, shared.media_type),
        _ => {
            let Some(index) = name
                .strip_suffix(".m4s")
                .and_then(|index| index.parse::<usize>().ok())
            else {
                return status(writer, "404 Not Found");
            };
            let made = match shared.remuxer.lock() {
                Ok(mut remuxer) if index < remuxer.segments().len() => remuxer.media_segment(index),
                Ok(_) => return status(writer, "404 Not Found"),
                Err(_) => return status(writer, "500 Internal Server Error"),
            };
            match made {
                Ok(bytes) => segment = bytes,
                Err(_) => return status(writer, "500 Internal Server Error"),
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
