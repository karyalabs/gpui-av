//! A loopback proxy for HLS that needs request headers (a media server's
//! token). A platform player that cannot send headers for HLS
//! (AVFoundation's resource loader cannot serve playlists) plays the
//! proxy's URL instead: the proxy fetches each playlist, key and segment
//! through a [`Fetch`] that adds the headers, and rewrites every URI in a
//! playlist to one of its own. Nothing is cached, so live playlists stay
//! live.
//!
//! It serves a plain media URL the same way, for a player that cannot send
//! headers at all, or not under the origin rule: everything but a playlist
//! is streamed through, ranges included.

use std::collections::HashMap;
use std::io::{self, BufReader, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crate::serve::{Failure, FailureKind, READ_TIMEOUT, read_request, token};

/// What a server answered to a [`Fetch::get`].
#[derive(Clone, Debug, Default)]
pub struct HttpResponse {
    /// The HTTP status.
    pub status: u16,
    /// `Content-Type`, when the server sent one.
    pub content_type: Option<String>,
    /// `Content-Range`, for a 206.
    pub content_range: Option<String>,
    /// The URL that answered, after any redirects: relative URIs in a
    /// playlist resolve against it.
    pub url: String,
    /// The body.
    pub body: Vec<u8>,
}

/// A response whose body is read as it arrives, from [`Fetch::open`].
pub struct HttpStream {
    /// The HTTP status.
    pub status: u16,
    /// `Content-Type`, when the server sent one.
    pub content_type: Option<String>,
    /// `Content-Range`, for a 206.
    pub content_range: Option<String>,
    /// `Content-Length`, when the server sent one.
    pub content_length: Option<u64>,
    /// The URL that answered, after any redirects.
    pub url: String,
    /// The body.
    pub body: Box<dyn Read + Send>,
}

/// Fetches a URL with the source's headers (on the source's origin only,
/// redirects included). It blocks; the proxy calls it on its own threads.
pub trait Fetch: Send + Sync + 'static {
    /// GETs `url`, for bytes `start` to `end` (inclusive, or to the end)
    /// when `range` is given.
    fn get(&self, url: &str, range: Option<(u64, Option<u64>)>) -> io::Result<HttpResponse>;

    /// GETs `url` like [`Fetch::get`], handing over the body as it
    /// arrives, so a whole film is never held in memory. By default it
    /// reads the whole body with `get`.
    fn open(&self, url: &str, range: Option<(u64, Option<u64>)>) -> io::Result<HttpStream> {
        let response = self.get(url, range)?;
        Ok(HttpStream {
            status: response.status,
            content_type: response.content_type,
            content_range: response.content_range,
            content_length: Some(response.body.len() as u64),
            url: response.url,
            body: Box::new(io::Cursor::new(response.body)),
        })
    }
}

/// The longest playlist the proxy reads: a long VOD playlist is a few MB.
const MAX_PLAYLIST: u64 = 64 << 20;

/// The upstream URLs the proxy has handed out, by number.
#[derive(Default)]
struct Table {
    urls: Vec<String>,
    numbers: HashMap<String, usize>,
}

impl Table {
    fn number(&mut self, url: &str) -> usize {
        if let Some(number) = self.numbers.get(url) {
            return *number;
        }
        self.urls.push(url.to_string());
        let number = self.urls.len() - 1;
        self.numbers.insert(url.to_string(), number);
        number
    }
}

struct Shared {
    fetch: Box<dyn Fetch>,
    table: Mutex<Table>,
    prefix: String,
    stop: Arc<AtomicBool>,
    failure: Mutex<Option<Failure>>,
}

impl Shared {
    /// The proxy path for an upstream URL: its number, and its last path
    /// part, for players that look at the name.
    fn local_path(&self, url: &str) -> String {
        let number = self
            .table
            .lock()
            .map(|mut table| table.number(url))
            .unwrap_or_default();
        let name = url
            .split(['?', '#'])
            .next()
            .and_then(|path| path.rsplit('/').next())
            .filter(|name| !name.is_empty())
            .unwrap_or("media");
        let name: String = name
            .chars()
            .filter(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
            })
            .collect();
        format!("{}/{number}/{name}", self.prefix)
    }

    fn keep_failure(&self, failure: Failure) {
        if let Ok(mut kept) = self.failure.lock() {
            kept.get_or_insert(failure);
        }
    }
}

/// Serves an HLS source on `127.0.0.1` through a [`Fetch`] until dropped.
///
/// The URL holds a random token, so other programs on the machine cannot
/// guess it, and it serves only URLs it has handed out.
pub struct HlsProxy {
    url: String,
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
}

impl HlsProxy {
    /// Starts the proxy for the playlist at `url`.
    pub fn start(url: &str, fetch: impl Fetch) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared {
            fetch: Box::new(fetch),
            table: Mutex::default(),
            prefix: format!("/{}", token()),
            stop: stop.clone(),
            failure: Mutex::default(),
        });
        let first = shared.local_path(url);
        let accept = thread::Builder::new()
            .name("gpui-av-hls-proxy".into())
            .spawn({
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
                            .name("gpui-av-hls-proxy-request".into())
                            .spawn(move || serve(stream, &shared));
                        if spawned.is_err() {
                            continue;
                        }
                    }
                }
            })?;
        Ok(Self {
            url: format!("http://{address}{first}"),
            address,
            stop,
            accept: Some(accept),
            shared,
        })
    }

    /// The playlist's URL on the proxy.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The first request upstream that failed, if one did: a player sees
    /// only the proxy's own answer.
    pub fn failure(&self) -> Option<Failure> {
        self.shared
            .failure
            .lock()
            .ok()
            .and_then(|kept| kept.clone())
    }
}

impl Drop for HlsProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accepting thread so it sees the flag.
        if let Err(error) = TcpStream::connect(self.address) {
            eprintln!("gpui-av-remux: waking the HLS proxy to stop failed: {error}");
        }
        if let Some(accept) = self.accept.take()
            && accept.join().is_err()
        {
            eprintln!("gpui-av-remux: stopping the HLS proxy failed: its thread panicked");
        }
    }
}

fn serve(stream: TcpStream, shared: &Shared) {
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
        if respond(
            &mut writer,
            &request.method,
            &request.path,
            request.range,
            shared,
        )
        .is_err()
            || !keep_alive
            || shared.stop.load(Ordering::SeqCst)
        {
            break;
        }
    }
    if let Ok(stream) = writer.into_inner() {
        stream.shutdown(Shutdown::Both).ok();
    }
}

fn respond(
    writer: &mut impl Write,
    method: &str,
    path: &str,
    range: Option<(u64, Option<u64>)>,
    shared: &Shared,
) -> io::Result<()> {
    if method != "GET" && method != "HEAD" {
        return status(writer, "405 Method Not Allowed");
    }
    let upstream = path
        .strip_prefix(shared.prefix.as_str())
        .and_then(|rest| rest.strip_prefix('/'))
        .and_then(|rest| rest.split('/').next())
        .and_then(|number| number.parse::<usize>().ok())
        .and_then(|number| {
            let table = shared.table.lock().ok()?;
            table.urls.get(number).cloned()
        });
    let Some(upstream) = upstream else {
        return status(writer, "404 Not Found");
    };
    let fail = |error: io::Error| {
        shared.keep_failure(Failure {
            kind: FailureKind::Io(error.kind()),
            message: format!("{upstream}: {error}"),
        });
    };
    let mut response = match shared.fetch.open(&upstream, range) {
        Ok(response) => response,
        Err(error) => {
            fail(error);
            return status(writer, "502 Bad Gateway");
        }
    };
    if !(200..300).contains(&response.status) {
        let kind = match response.status {
            401 | 403 => io::ErrorKind::PermissionDenied,
            404 | 410 => io::ErrorKind::NotFound,
            _ => io::ErrorKind::Other,
        };
        shared.keep_failure(Failure {
            kind: FailureKind::Io(kind),
            message: format!("{upstream}: the server answered {}", response.status),
        });
        return status(writer, &format!("{} Upstream", response.status));
    }
    // Enough of the body to tell a playlist: `#EXTM3U` after a BOM.
    let mut start = Vec::with_capacity(16);
    if let Err(error) = (&mut response.body).take(10).read_to_end(&mut start) {
        fail(error);
        return status(writer, "502 Bad Gateway");
    }
    if is_playlist(&start) {
        return respond_playlist(writer, method, &upstream, range, response, start, shared);
    }
    match (response.status, &response.content_range) {
        (206, Some(content_range)) => write!(
            writer,
            "HTTP/1.1 206 Partial Content\r\nContent-Range: {content_range}\r\n"
        )?,
        _ => write!(writer, "HTTP/1.1 200 OK\r\n")?,
    }
    if let Some(content_type) = &response.content_type {
        write!(writer, "Content-Type: {content_type}\r\n")?;
    }
    match response.content_length {
        Some(length) => write!(writer, "Content-Length: {length}\r\n")?,
        // The end of the body is where the connection closes.
        None => write!(writer, "Connection: close\r\n")?,
    }
    write!(
        writer,
        "Accept-Ranges: bytes\r\nCache-Control: no-store\r\n\r\n"
    )?;
    if method == "GET" {
        writer.write_all(&start)?;
        // A failure upstream mid-body cuts the answer short, which the
        // player sees as the connection ending early.
        io::copy(&mut response.body, writer)?;
    }
    writer.flush()?;
    if response.content_length.is_none() {
        return Err(io::Error::other("the body ended with the connection"));
    }
    Ok(())
}

/// Answers with the playlist in `response`, whose body so far is `start`,
/// rewritten.
fn respond_playlist(
    writer: &mut impl Write,
    method: &str,
    upstream: &str,
    range: Option<(u64, Option<u64>)>,
    mut response: HttpStream,
    start: Vec<u8>,
    shared: &Shared,
) -> io::Result<()> {
    let mut body = start;
    let read = if range.is_some() && response.status == 206 {
        // A player may ask for a playlist in ranges (Media Foundation
        // does): a playlist is rewritten whole, so it is fetched whole.
        shared.fetch.get(upstream, None).map(|whole| {
            body = whole.body;
            response.url = whole.url;
        })
    } else {
        (&mut response.body)
            .take(MAX_PLAYLIST)
            .read_to_end(&mut body)
            .map(drop)
    };
    if let Err(error) = read {
        shared.keep_failure(Failure {
            kind: FailureKind::Io(error.kind()),
            message: format!("{upstream}: {error}"),
        });
        return status(writer, "502 Bad Gateway");
    }
    let base = if response.url.is_empty() {
        upstream
    } else {
        response.url.as_str()
    };
    let text = String::from_utf8_lossy(&body);
    let rewritten = rewrite_playlist(&text, base, |url| shared.local_path(url));
    write!(
        writer,
        "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.apple.mpegurl\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\r\n",
        rewritten.len()
    )?;
    if method == "GET" {
        writer.write_all(rewritten.as_bytes())?;
    }
    writer.flush()
}

fn status(writer: &mut impl Write, status: &str) -> io::Result<()> {
    write!(writer, "HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n")?;
    writer.flush()
}

/// Every HLS playlist starts with `#EXTM3U` (RFC 8216, 4.3.1.1), whatever
/// type the server says it is.
fn is_playlist(body: &[u8]) -> bool {
    body.strip_prefix("\u{feff}".as_bytes())
        .unwrap_or(body)
        .starts_with(b"#EXTM3U")
}

/// The playlist with each URI (a line of its own, or a tag's `URI="…"`)
/// resolved against `base` and passed through `local`. URIs of schemes
/// other than `http` and `https` (`skd:`, `data:`) stay as they are.
pub(crate) fn rewrite_playlist(
    text: &str,
    base: &str,
    mut local: impl FnMut(&str) -> String,
) -> String {
    let mut out = String::with_capacity(text.len() + 256);
    let mut map = |uri: &str| -> Option<String> {
        let absolute = resolve_url(base, uri)?;
        let lower = absolute.to_ascii_lowercase();
        (lower.starts_with("http://") || lower.starts_with("https://")).then(|| local(&absolute))
    };
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            out.push('\n');
            continue;
        }
        if !trimmed.starts_with('#') {
            out += &map(trimmed).unwrap_or_else(|| trimmed.to_string());
            out.push('\n');
            continue;
        }
        let mut rest = line;
        while let Some(at) = rest.find("URI=\"") {
            let (before, after) = rest.split_at(at + 5);
            out += before;
            let Some(end) = after.find('"') else {
                rest = after;
                break;
            };
            let uri = &after[..end];
            out += &map(uri).unwrap_or_else(|| uri.to_string());
            rest = &after[end..];
        }
        out += rest;
        out.push('\n');
    }
    out
}

/// `reference` resolved against the absolute URL `base` (RFC 3986,
/// section 5.2), for the URIs in a playlist.
pub fn resolve_url(base: &str, reference: &str) -> Option<String> {
    let reference = reference.trim();
    if let Some((scheme, _)) = reference.split_once(':')
        && !scheme.is_empty()
        && scheme
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "+-.".contains(character))
        && scheme
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphabetic())
    {
        return Some(reference.to_string());
    }
    let (scheme, rest) = base.split_once("://")?;
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, base_path) = rest.split_at(authority_end);
    let origin = format!("{scheme}://{authority}");
    if let Some(network) = reference.strip_prefix("//") {
        return Some(format!("{scheme}://{network}"));
    }
    let base_path = base_path.split('#').next().unwrap_or_default();
    let (base_path, base_query) = match base_path.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (base_path, None),
    };
    if reference.is_empty() {
        return Some(match base_query {
            Some(query) => format!("{origin}{base_path}?{query}"),
            None => format!("{origin}{base_path}"),
        });
    }
    if reference.starts_with('#') {
        return Some(format!("{origin}{base_path}{reference}"));
    }
    if reference.starts_with('?') {
        return Some(format!("{origin}{base_path}{reference}"));
    }
    let (reference_path, suffix) = match reference.find(['?', '#']) {
        Some(at) => reference.split_at(at),
        None => (reference, ""),
    };
    let merged = if reference_path.starts_with('/') {
        reference_path.to_string()
    } else {
        let directory = &base_path[..base_path.rfind('/').map_or(0, |at| at + 1)];
        let directory = if directory.is_empty() { "/" } else { directory };
        format!("{directory}{reference_path}")
    };
    Some(format!("{origin}{}{suffix}", remove_dot_segments(&merged)))
}

fn remove_dot_segments(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    let parts: Vec<&str> = path.split('/').collect();
    for (index, part) in parts.iter().enumerate() {
        let last = index == parts.len() - 1;
        match *part {
            "." => {
                if last {
                    segments.push("");
                }
            }
            ".." => {
                if segments.len() > 1 {
                    segments.pop();
                }
                if last {
                    segments.push("");
                }
            }
            part => segments.push(part),
        }
    }
    let joined = segments.join("/");
    if joined.starts_with('/') {
        joined
    } else {
        format!("/{joined}")
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use super::*;

    #[test]
    fn resolves_like_rfc_3986() {
        let base = "http://a/b/c/d;p?q";
        for (reference, expected) in [
            ("g:h", "g:h"),
            ("g", "http://a/b/c/g"),
            ("./g", "http://a/b/c/g"),
            ("g/", "http://a/b/c/g/"),
            ("/g", "http://a/g"),
            ("//g", "http://g"),
            ("?y", "http://a/b/c/d;p?y"),
            ("g?y", "http://a/b/c/g?y"),
            ("#s", "http://a/b/c/d;p#s"),
            ("g#s", "http://a/b/c/g#s"),
            ("..", "http://a/b/"),
            ("../g", "http://a/b/g"),
            ("../..", "http://a/"),
            ("../../g", "http://a/g"),
            ("../../../g", "http://a/g"),
            ("", "http://a/b/c/d;p?q"),
        ] {
            assert_eq!(
                resolve_url(base, reference).as_deref(),
                Some(expected),
                "{reference}"
            );
        }
    }

    #[test]
    fn rewrites_every_uri_in_a_playlist() {
        let playlist = "#EXTM3U\n\
            #EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x1\n\
            #EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://asset\"\n\
            #EXT-X-MAP:URI=\"init.mp4\"\n\
            #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",URI=\"https://cdn.test/audio/en.m3u8\"\n\
            #EXTINF:4.0,\n\
            seg/0.m4s?token=x\n\
            \n\
            #EXTINF:4.0,\n\
            /abs/1.m4s\n";
        let mut seen = Vec::new();
        let out = rewrite_playlist(playlist, "https://media.test/v/1/main.m3u8?api=2", |url| {
            seen.push(url.to_string());
            format!("/p/{}", seen.len() - 1)
        });
        assert_eq!(
            seen,
            [
                "https://media.test/v/1/key.bin",
                "https://media.test/v/1/init.mp4",
                "https://cdn.test/audio/en.m3u8",
                "https://media.test/v/1/seg/0.m4s?token=x",
                "https://media.test/abs/1.m4s",
            ]
        );
        assert!(out.contains("#EXT-X-KEY:METHOD=AES-128,URI=\"/p/0\",IV=0x1\n"));
        assert!(out.contains("URI=\"skd://asset\""));
        assert!(out.contains("#EXT-X-MAP:URI=\"/p/1\"\n"));
        assert!(out.contains("GROUP-ID=\"a\",URI=\"/p/2\"\n"));
        assert!(out.ends_with("/p/3\n\n#EXTINF:4.0,\n/p/4\n"));
    }

    /// A fake upstream: a master playlist, a media playlist reached
    /// through a redirect, and a segment.
    struct Upstream;

    impl Fetch for Upstream {
        fn get(&self, url: &str, _range: Option<(u64, Option<u64>)>) -> io::Result<HttpResponse> {
            let (status, url, body): (u16, &str, &[u8]) = match url {
                "https://media.test/master.m3u8" => (
                    200,
                    url,
                    b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nvideo/index.m3u8\n",
                ),
                // Redirected to a CDN: its segments resolve against it.
                "https://media.test/video/index.m3u8" => (
                    200,
                    "https://cdn.test/v/index.m3u8",
                    b"#EXTM3U\n#EXTINF:1,\n0.ts\n#EXT-X-ENDLIST\n",
                ),
                "https://cdn.test/v/0.ts" => (206, url, b"0123456789"),
                _ => (401, url, b""),
            };
            Ok(HttpResponse {
                status,
                content_type: None,
                content_range: (status == 206).then(|| "bytes 2-5/10".to_string()),
                url: url.to_string(),
                body: body.to_vec(),
            })
        }
    }

    fn get(url: &str, range: Option<&str>) -> (String, Vec<u8>) {
        let rest = url.strip_prefix("http://").expect("an http URL");
        let (host, path) = rest.split_at(rest.find('/').expect("a path"));
        let mut stream = TcpStream::connect(host).expect("connect");
        let range = range
            .map(|range| format!("Range: {range}\r\n"))
            .unwrap_or_default();
        write!(stream, "GET {path} HTTP/1.0\r\n{range}\r\n").expect("send");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).expect("read");
        let split = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("headers end");
        let head = String::from_utf8_lossy(&response[..split]).to_string();
        (head, response[split + 4..].to_vec())
    }

    #[test]
    fn proxies_playlists_and_segments() {
        let upstream = Upstream;
        let proxy = HlsProxy::start("https://media.test/master.m3u8", upstream).expect("start");
        let local = |path: &str| {
            let host = proxy.url()[7..]
                .split('/')
                .next()
                .unwrap_or_default()
                .to_string();
            format!("http://{host}{path}")
        };
        let (head, master) = get(proxy.url(), None);
        assert!(head.contains("200"), "{head}");
        assert!(head.contains("application/vnd.apple.mpegurl"), "{head}");
        let master = String::from_utf8(master).expect("text");
        let media_path = master.lines().nth(2).expect("a variant").to_string();
        assert!(media_path.ends_with("/index.m3u8"), "{master}");
        // Asked for in a range, a playlist still comes whole and rewritten.
        let (head, media) = get(&local(&media_path), Some("bytes=0-"));
        assert!(head.contains("200"), "{head}");
        let media = String::from_utf8(media).expect("text");
        let segment_path = media.lines().nth(2).expect("a segment").to_string();
        let (head, segment) = get(&local(&segment_path), Some("bytes=2-5"));
        assert!(head.contains("206"), "{head}");
        assert!(head.contains("Content-Range: bytes 2-5/10"), "{head}");
        assert_eq!(segment, b"0123456789");
        // Nothing the proxy did not hand out is served.
        let (head, _) = get(&local("/elsewhere/0/x"), None);
        assert!(head.contains("404"), "{head}");
        let token = proxy.url()[7..]
            .split('/')
            .nth(1)
            .unwrap_or_default()
            .to_string();
        let (head, _) = get(&local(&format!("/{token}/99/x")), None);
        assert!(head.contains("404"), "{head}");
        assert!(proxy.failure().is_none());
    }

    /// Hands over bodies as streams of unknown length.
    struct Streaming;

    impl Fetch for Streaming {
        fn get(&self, _url: &str, _range: Option<(u64, Option<u64>)>) -> io::Result<HttpResponse> {
            Err(io::Error::other("only open"))
        }

        fn open(&self, url: &str, range: Option<(u64, Option<u64>)>) -> io::Result<HttpStream> {
            let body: Vec<u8> = match url {
                "https://media.test/film.mkv" => (0..100_000u32).map(|byte| byte as u8).collect(),
                _ => b"abc".to_vec(),
            };
            Ok(HttpStream {
                status: if range.is_some() { 206 } else { 200 },
                content_type: Some("video/x-matroska".into()),
                content_range: range.map(|_| format!("bytes 0-{}/{}", body.len() - 1, body.len())),
                content_length: None,
                url: url.to_string(),
                body: Box::new(io::Cursor::new(body)),
            })
        }
    }

    #[test]
    fn streams_media_that_is_not_a_playlist() {
        let proxy = HlsProxy::start("https://media.test/film.mkv", Streaming).expect("start");
        assert!(proxy.url().ends_with("/film.mkv"), "{}", proxy.url());
        let (head, body) = get(proxy.url(), Some("bytes=0-"));
        assert!(head.contains("206"), "{head}");
        assert!(
            head.contains("Content-Range: bytes 0-99999/100000"),
            "{head}"
        );
        assert!(head.contains("video/x-matroska"), "{head}");
        assert!(head.contains("Connection: close"), "{head}");
        assert_eq!(body.len(), 100_000);
        assert_eq!(body[1000], (1000u32 % 256) as u8);
        let short = HlsProxy::start("https://media.test/tiny", Streaming).expect("start");
        let (head, body) = get(short.url(), None);
        assert!(head.contains("200"), "{head}");
        assert_eq!(body, b"abc");
    }

    #[test]
    fn keeps_the_first_upstream_failure() {
        let upstream = Upstream;
        let proxy = HlsProxy::start("https://media.test/private.m3u8", upstream).expect("start");
        let (head, _) = get(proxy.url(), None);
        assert!(head.contains("401"), "{head}");
        let failure = proxy.failure().expect("a failure");
        assert_eq!(
            failure.kind,
            FailureKind::Io(io::ErrorKind::PermissionDenied)
        );
    }
}
