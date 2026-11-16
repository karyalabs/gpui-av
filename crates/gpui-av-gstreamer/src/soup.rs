//! Fetching a URL with request headers through libsoup 3, for the loopback
//! proxy a source with headers plays through.
//!
//! `souphttpsrc` sends its `extra-headers` with every request, and libsoup
//! follows a redirect to another origin with all of them but
//! `Authorization`: a media server's token in a header such as
//! `X-Emby-Token` would reach a CDN. Here redirects are followed by hand,
//! and the headers go to the source's origin only, as on the other
//! backends.

use std::ffi::{CString, c_void};
use std::io::{self, Read};
use std::ptr;

use gpui_av_remux::{Fetch, HttpResponse, HttpStream, resolve_url};

use crate::ffi::{self, SoupApi};

const MAX_REDIRECTS: usize = 10;
/// `SOUP_MESSAGE_NO_REDIRECT`.
const NO_REDIRECT: u32 = 1 << 1;

/// The scheme, host and port of an `http` or `https` URL, lower case.
fn origin(url: &str) -> Option<(String, String, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default = match scheme.as_str() {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let (host, port) = match authority.strip_prefix('[') {
        Some(bracketed) => {
            let (host, after) = bracketed.split_once(']')?;
            (host, after.strip_prefix(':'))
        }
        None => match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        },
    };
    if host.is_empty() {
        return None;
    }
    let port = match port {
        Some(port) if !port.is_empty() => port.parse().ok()?,
        _ => default,
    };
    Some((scheme, host.to_ascii_lowercase(), port))
}

/// Fetches with the source's headers, on its origin only.
pub(crate) struct SoupFetch {
    api: &'static SoupApi,
    origin: (String, String, u16),
    headers: Vec<(CString, CString)>,
}

impl SoupFetch {
    /// `None` when libsoup 3 is not installed or `url` is not `http(s)`.
    pub(crate) fn new(url: &str, headers: &[(String, String)]) -> Option<Self> {
        let api = match ffi::soup_api() {
            Ok(api) => api,
            Err(error) => {
                eprintln!("gpui-av-gstreamer: {error}");
                return None;
            }
        };
        let headers = headers
            .iter()
            .filter(|(name, _)| !name.eq_ignore_ascii_case("range"))
            .filter_map(|(name, value)| {
                Some((
                    CString::new(name.as_str()).ok()?,
                    CString::new(value.as_str()).ok()?,
                ))
            })
            .collect();
        Some(Self {
            api,
            origin: origin(url)?,
            headers,
        })
    }

    /// One GET without following a redirect.
    fn send(&self, url: &str, range: Option<(u64, Option<u64>)>) -> io::Result<Body> {
        let invalid = || io::Error::new(io::ErrorKind::InvalidInput, format!("not a URL: {url}"));
        let text = CString::new(url).map_err(|_| invalid())?;
        let soup = &self.api.soup;
        // SAFETY: libsoup calls with valid strings; the session, message and
        // stream are owned by `Body`, which frees them on this thread.
        unsafe {
            let message = (soup.soup_message_new)(c"GET".as_ptr(), text.as_ptr());
            if message.is_null() {
                return Err(invalid());
            }
            let mut body = Body {
                api: self.api,
                session: (soup.soup_session_new)(),
                message,
                stream: ptr::null_mut(),
            };
            // Ranges must reach us as the server sent them.
            (soup.soup_session_remove_feature_by_type)(
                body.session,
                (soup.soup_content_decoder_get_type)(),
            );
            (soup.soup_message_set_flags)(message, NO_REDIRECT);
            let request = (soup.soup_message_get_request_headers)(message);
            if let Some((start, end)) = range {
                let value = match end {
                    Some(end) => format!("bytes={start}-{end}"),
                    None => format!("bytes={start}-"),
                };
                let value = CString::new(value).map_err(|_| invalid())?;
                (soup.soup_message_headers_append)(request, c"Range".as_ptr(), value.as_ptr());
            }
            if origin(url).as_ref() == Some(&self.origin) {
                for (name, value) in &self.headers {
                    (soup.soup_message_headers_append)(request, name.as_ptr(), value.as_ptr());
                }
            }
            let mut error = ptr::null_mut();
            body.stream =
                (soup.soup_session_send)(body.session, message, ptr::null_mut(), &mut error);
            if body.stream.is_null() {
                let message = ffi::take_error(&self.api.glib, error)
                    .map(|(_, _, message)| message)
                    .unwrap_or_else(|| "the request failed".into());
                return Err(io::Error::other(format!("{url}: {message}")));
            }
            Ok(body)
        }
    }
}

impl Fetch for SoupFetch {
    fn get(&self, url: &str, range: Option<(u64, Option<u64>)>) -> io::Result<HttpResponse> {
        let mut stream = self.open(url, range)?;
        let mut body = Vec::new();
        stream.body.read_to_end(&mut body)?;
        Ok(HttpResponse {
            status: stream.status,
            content_type: stream.content_type,
            content_range: stream.content_range,
            url: stream.url,
            body,
        })
    }

    fn open(&self, url: &str, range: Option<(u64, Option<u64>)>) -> io::Result<HttpStream> {
        let mut url = url.to_string();
        for _ in 0..=MAX_REDIRECTS {
            let body = self.send(&url, range)?;
            let status = body.status();
            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                url = body
                    .header(c"Location")
                    .and_then(|location| resolve_url(&url, &location))
                    .ok_or_else(|| io::Error::other("a redirect with no usable Location"))?;
                continue;
            }
            return Ok(HttpStream {
                status,
                content_type: body.header(c"Content-Type"),
                content_range: body.header(c"Content-Range"),
                content_length: body
                    .header(c"Content-Length")
                    .and_then(|length| length.trim().parse().ok()),
                url,
                body: Box::new(body),
            });
        }
        Err(io::Error::other("too many redirects"))
    }
}

/// A response being read: its session, message and body stream.
struct Body {
    api: &'static SoupApi,
    session: *mut c_void,
    message: *mut c_void,
    stream: *mut c_void,
}

// SAFETY: the proxy reads a body on the thread that sent its request and
// drops it there; it is only moved, never shared.
unsafe impl Send for Body {}

impl Body {
    fn status(&self) -> u16 {
        // SAFETY: a message that was sent.
        let status = unsafe { (self.api.soup.soup_message_get_status)(self.message) };
        u16::try_from(status).unwrap_or(0)
    }

    fn header(&self, name: &std::ffi::CStr) -> Option<String> {
        // SAFETY: the response headers of a message that was sent; the
        // value is copied before anything else touches them.
        unsafe {
            let headers = (self.api.soup.soup_message_get_response_headers)(self.message);
            ffi::string((self.api.soup.soup_message_headers_get_one)(
                headers,
                name.as_ptr(),
            ))
        }
    }
}

impl Read for Body {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let mut error = ptr::null_mut();
        // SAFETY: an open stream, and a buffer of the length given.
        let read = unsafe {
            (self.api.gio.g_input_stream_read)(
                self.stream,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                ptr::null_mut(),
                &mut error,
            )
        };
        match usize::try_from(read) {
            Ok(read) => Ok(read),
            Err(_) => {
                // SAFETY: the error the failed read set, ours to free.
                let message = unsafe { ffi::take_error(&self.api.glib, error) }
                    .map(|(_, _, message)| message)
                    .unwrap_or_else(|| "reading the body failed".into());
                Err(io::Error::other(message))
            }
        }
    }
}

impl Drop for Body {
    fn drop(&mut self) {
        // SAFETY: each object is ours, released once, on this thread.
        unsafe {
            if !self.stream.is_null() {
                (self.api.gio.g_input_stream_close)(self.stream, ptr::null_mut(), ptr::null_mut());
                (self.api.gobject.g_object_unref)(self.stream);
            }
            (self.api.gobject.g_object_unref)(self.message);
            (self.api.gobject.g_object_unref)(self.session);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use gpui_av_remux::{Fetch, HlsProxy};

    use super::{SoupFetch, origin};

    const TOKEN: &str = "secret";

    /// A server where `/hop/…` (with the token) redirects to `/open/…` on
    /// another origin, which serves a body. It logs each request's path
    /// and token.
    /// Each request's path and token.
    type Log = Arc<Mutex<Vec<(String, Option<String>)>>>;

    fn server(clip: Vec<u8>) -> (u16, Log) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let log = Arc::new(Mutex::new(Vec::new()));
        std::thread::spawn({
            let log = log.clone();
            move || {
                for stream in listener.incoming().flatten() {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut writer = stream;
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() {
                        continue;
                    }
                    let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                    let (mut token, mut host, mut range) = (None, String::new(), None);
                    loop {
                        let mut header = String::new();
                        if reader.read_line(&mut header).unwrap_or(0) == 0 {
                            break;
                        }
                        let header = header.trim_end();
                        if header.is_empty() {
                            break;
                        }
                        if let Some((name, value)) = header.split_once(':') {
                            let value = value.trim().to_string();
                            match name.to_ascii_lowercase().as_str() {
                                "x-emby-token" => token = Some(value),
                                "host" => host = value,
                                "range" => range = Some(value),
                                _ => {}
                            }
                        }
                    }
                    log.lock().unwrap().push((path.clone(), token.clone()));
                    let reply = if path.starts_with("/hop/") && token.as_deref() == Some(TOKEN) {
                        let other = if host.starts_with("127.0.0.1") {
                            "localhost"
                        } else {
                            "127.0.0.1"
                        };
                        format!(
                            "HTTP/1.1 302 Found\r\nLocation: http://{other}:{port}/open/clip\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        )
                    } else if path.starts_with("/open/") && token.is_none() {
                        let size = clip.len();
                        let parsed = range.as_deref().and_then(|range| {
                            let (start, end) = range.strip_prefix("bytes=")?.split_once('-')?;
                            let start: usize = start.parse().ok()?;
                            let end = end.parse().unwrap_or(size - 1).min(size - 1);
                            (start <= end).then_some((start, end))
                        });
                        let head = match parsed {
                            Some((start, end)) => format!(
                                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{size}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                end - start + 1
                            ),
                            None => format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {size}\r\nConnection: close\r\n\r\n"
                            ),
                        };
                        let (start, end) = parsed.unwrap_or((0, size - 1));
                        writer.write_all(head.as_bytes()).ok();
                        writer.write_all(&clip[start..=end]).ok();
                        continue;
                    } else {
                        "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                    };
                    writer.write_all(reply.as_bytes()).ok();
                }
            }
        });
        (port, log)
    }

    fn fetch(url: &str) -> Option<SoupFetch> {
        let headers = [("X-Emby-Token".to_string(), TOKEN.to_string())];
        let fetch = SoupFetch::new(url, &headers);
        if fetch.is_none() {
            eprintln!("skipped: libsoup 3 is not installed");
        }
        fetch
    }

    #[test]
    fn headers_stay_on_the_source_origin() {
        let (port, log) = server(b"0123456789".to_vec());
        let url = format!("http://127.0.0.1:{port}/hop/clip");
        let Some(fetch) = fetch(&url) else { return };
        let response = fetch.get(&url, None).expect("get");
        assert_eq!(
            (response.status, response.body.as_slice()),
            (200, &b"0123456789"[..])
        );
        assert_eq!(response.url, format!("http://localhost:{port}/open/clip"));
        let ranged = fetch.get(&url, Some((2, Some(5)))).expect("ranged get");
        assert_eq!(ranged.status, 206);
        assert_eq!(ranged.content_range.as_deref(), Some("bytes 2-5/10"));
        assert_eq!(ranged.body, b"2345");
        let log = log.lock().unwrap().clone();
        assert_eq!(log.len(), 4, "{log:?}");
        for (path, token) in log {
            let expected = path.starts_with("/hop/").then(|| TOKEN.to_string());
            assert_eq!(token, expected, "{path}");
        }
    }

    #[test]
    fn plays_through_the_proxy() {
        let (port, log) = server(b"0123456789".to_vec());
        let url = format!("http://127.0.0.1:{port}/hop/clip");
        let Some(fetch) = fetch(&url) else { return };
        let proxy = HlsProxy::start(&url, fetch).expect("proxy");
        let rest = proxy.url().strip_prefix("http://").unwrap();
        let (host, path) = rest.split_at(rest.find('/').unwrap());
        let mut stream = std::net::TcpStream::connect(host).unwrap();
        write!(stream, "GET {path} HTTP/1.0\r\n\r\n").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.ends_with("0123456789"), "{response}");
        let log = log.lock().unwrap().clone();
        assert!(
            log.iter().any(|(path, _)| path.starts_with("/open/")),
            "{log:?}"
        );
        assert!(
            log.iter()
                .all(|(path, token)| path.starts_with("/hop/") == token.is_some()),
            "{log:?}"
        );
    }

    #[test]
    fn origins_are_scheme_host_and_port() {
        let base = origin("http://A.test/x").unwrap();
        assert_eq!(base, ("http".into(), "a.test".into(), 80));
        assert_eq!(origin("http://a.test:80/y").as_ref(), Some(&base));
        assert_ne!(origin("https://a.test/x").as_ref(), Some(&base));
        assert_ne!(origin("http://a.test:8096/x").as_ref(), Some(&base));
        assert_eq!(
            origin("https://user:pw@[::1]:8920/a?b").unwrap(),
            ("https".into(), "::1".into(), 8920)
        );
        assert!(origin("file:///a").is_none());
    }

    /// playbin plays a clip with a token through the proxy, seeking with
    /// ranges, and the token never leaves the source's origin. Needs a clip
    /// (`GPUI_AV_CLIP`) and `gst-launch-1.0`; nothing is played out loud.
    #[test]
    #[ignore = "needs GPUI_AV_CLIP and gst-launch-1.0"]
    fn playbin_plays_through_the_proxy() {
        let clip =
            std::fs::read(std::env::var("GPUI_AV_CLIP").expect("GPUI_AV_CLIP")).expect("clip");
        let (port, log) = server(clip);
        let url = format!("http://127.0.0.1:{port}/hop/clip");
        let fetch = fetch(&url).expect("libsoup 3");
        let proxy = HlsProxy::start(&url, fetch).expect("proxy");
        let status = std::process::Command::new("gst-launch-1.0")
            .args(["-q", "playbin3", &format!("uri={}", proxy.url())])
            .args([
                "video-sink=fakesink sync=false",
                "audio-sink=fakesink sync=false",
            ])
            .status()
            .expect("gst-launch-1.0");
        assert!(status.success(), "{status}");
        let log = log.lock().unwrap().clone();
        eprintln!("{} requests upstream", log.len());
        assert!(
            log.iter().any(|(path, _)| path.starts_with("/open/")),
            "{log:?}"
        );
        assert!(
            log.iter()
                .all(|(path, token)| path.starts_with("/hop/") == token.is_some()),
            "{log:?}"
        );
    }
}
