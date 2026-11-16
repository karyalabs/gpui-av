//! AVFoundation playback. Everything Apple lives below this module.

mod errors;
mod info;
mod loader;
mod probe;
mod remote;
mod session;
mod tracks;

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use gpui::App;
use gpui_av::{Backend, Error, ErrorKind, EventSink, Location, Session, Source};
use objc2::rc::Retained;
use objc2::{AnyThread, MainThreadMarker};
use objc2_av_foundation::AVURLAsset;
use objc2_foundation::{NSString, NSURL};

/// Plays media with AVFoundation's `AVPlayer` and draws it with GPUI's
/// surface. macOS only.
///
/// Sources: file paths, and `http`/`https` URLs with request headers. A URL
/// with headers is loaded by the backend's own requests, not AVFoundation's:
/// every header goes with every request for the media, range requests
/// included, and with redirects that stay on the URL's origin (scheme, host
/// and port); a redirect to another origin gets none of them. HLS playlists
/// with headers are not supported yet and fail as `Unsupported`.
///
/// AVFoundation opens no Matroska or WebM. A file in either is remuxed to
/// fragmented MP4 as it plays (`gpui-av-remux`, nothing is decoded) and
/// served to AVFoundation as HLS from `127.0.0.1`, so its own decoders
/// still play it. A URL whose path ends in `.mkv` or `.webm` (as
/// Jellyfin's direct play URLs do) is remuxed the same way, read with range
/// requests that carry the source's headers; the server must take ranges.
#[derive(Clone, Copy, Debug, Default)]
pub struct AvFoundationBackend;

impl AvFoundationBackend {
    /// The backend. It holds no state; one is enough for an app.
    pub fn new() -> Self {
        Self
    }
}

impl Backend for AvFoundationBackend {
    fn name(&self) -> &'static str {
        "avfoundation"
    }

    fn presenter(&self) -> &'static str {
        "surface"
    }

    fn open(
        &self,
        source: Source,
        events: EventSink,
        cx: &mut App,
    ) -> Result<Box<dyn Session>, Error> {
        let mtm = MainThreadMarker::new().ok_or_else(|| {
            Error::new(
                ErrorKind::Other,
                "the AVFoundation backend must be opened on the main thread",
            )
        })?;
        let local = match proxied(&source)? {
            Some(proxy) => Some(Local::Proxy(proxy)),
            None => remuxed(&source)?.map(Local::Remux),
        };
        let (asset, loader, remuxed) = match local {
            Some(remuxed) => {
                let url = web_url(remuxed.url())?;
                // SAFETY: `url` is a valid NSURL, and there are no options.
                let asset =
                    unsafe { AVURLAsset::initWithURL_options(AVURLAsset::alloc(), &url, None) };
                (asset, None, Some(remuxed))
            }
            None => {
                let (asset, loader) = asset_for(&source)?;
                (asset, loader, None)
            }
        };
        let http = source.as_url().map(|url| probe::HttpSource {
            url: url.to_string(),
            headers: source.headers().to_vec(),
        });
        Ok(Box::new(session::AvSession::new(
            asset, loader, remuxed, http, events, mtm, cx,
        )))
    }
}

/// A local server a source plays from.
pub(crate) enum Local {
    /// A Matroska or WebM source, remuxed and served as HLS.
    Remux(gpui_av_remux::HlsServer),
    /// HLS with request headers, through a proxy that sends them.
    Proxy(gpui_av_remux::HlsProxy),
}

impl Local {
    fn url(&self) -> &str {
        match self {
            Self::Remux(server) => server.url(),
            Self::Proxy(proxy) => proxy.url(),
        }
    }

    /// Why serving failed, when it did: AVFoundation saw only the local
    /// server's answer.
    pub(crate) fn failure(&self) -> Option<gpui_av_remux::Failure> {
        match self {
            Self::Remux(server) => server.failure(),
            Self::Proxy(proxy) => proxy.failure(),
        }
    }

    /// What the remuxer read from the file's header.
    pub(crate) fn summary(&self) -> Option<gpui_av_remux::Summary> {
        match self {
            Self::Remux(server) => server.summary(),
            Self::Proxy(_) => None,
        }
    }
}

/// For an HLS URL with request headers, a loopback proxy that fetches the
/// playlists, keys and segments with them (on the URL's origin only).
/// AVFoundation's own HLS cannot send headers, and a resource loader cannot
/// serve a playlist's segments.
fn proxied(source: &Source) -> Result<Option<gpui_av_remux::HlsProxy>, Error> {
    let Some(url) = source.as_url() else {
        return Ok(None);
    };
    if source.headers().is_empty() || !names_playlist(url) {
        return Ok(None);
    }
    let requests = remote::Requests::new(url, source.headers()).ok_or_else(|| {
        Error::new(
            ErrorKind::Unsupported,
            format!("not an http or https URL: {url}"),
        )
    })?;
    gpui_av_remux::HlsProxy::start(url, requests)
        .map(Some)
        .map_err(|error| {
            Error::new(
                ErrorKind::Other,
                format!("cannot start the local proxy for {url}: {error}"),
            )
        })
}

fn names_playlist(url: &str) -> bool {
    url.split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase()
        .ends_with(".m3u8")
}

/// For a file that is Matroska or WebM, or a URL named like one, its
/// remuxer serving it as HLS.
fn remuxed(source: &Source) -> Result<Option<gpui_av_remux::HlsServer>, Error> {
    if let Some(url) = source.as_url() {
        if !names_matroska(url) {
            return Ok(None);
        }
        let ranges = remote::UrlRanges::new(url, source.headers()).ok_or_else(|| {
            Error::new(
                ErrorKind::Unsupported,
                format!("not an http or https URL: {url}"),
            )
        })?;
        // Reading the header takes a few requests: not on the main thread.
        let server = gpui_av_remux::HlsServer::open_in_background(move || {
            gpui_av_remux::Remuxer::open(gpui_av_remux::RangeReader::new(ranges))
        })
        .map_err(|error| {
            Error::new(
                ErrorKind::Other,
                format!("cannot start the local server for {url}: {error}"),
            )
        })?;
        return Ok(Some(server));
    }
    let Some(path) = source.path() else {
        return Ok(None);
    };
    let Ok(mut file) = File::open(path) else {
        // `asset_for` reports a missing file.
        return Ok(None);
    };
    let mut start = [0; 4];
    if file.read_exact(&mut start).is_err() || !gpui_av_remux::is_matroska(&start) {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(0)).map_err(|error| {
        Error::new(
            ErrorKind::Other,
            format!("cannot read {}: {error}", path.display()),
        )
    })?;
    let remuxer = gpui_av_remux::Remuxer::open(BufReader::new(file)).map_err(|error| {
        let kind = match error {
            gpui_av_remux::RemuxError::Invalid(_) => ErrorKind::Decode,
            gpui_av_remux::RemuxError::Io(_) => ErrorKind::Other,
            _ => ErrorKind::Unsupported,
        };
        Error::new(kind, error.to_string())
    })?;
    let server = gpui_av_remux::HlsServer::start(remuxer).map_err(|error| {
        Error::new(
            ErrorKind::Other,
            format!(
                "cannot start the local server for {}: {error}",
                path.display()
            ),
        )
    })?;
    Ok(Some(server))
}

/// Whether a URL's path ends in `.mkv` or `.webm`.
fn names_matroska(url: &str) -> bool {
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    path.ends_with(".mkv") || path.ends_with(".webm")
}

/// The error for a remuxed source that could not be served.
fn remux_failure(failure: gpui_av_remux::Failure) -> Error {
    use gpui_av_remux::FailureKind;
    let kind = match failure.kind {
        FailureKind::Unsupported => ErrorKind::Unsupported,
        FailureKind::Damaged => ErrorKind::Decode,
        FailureKind::Io(std::io::ErrorKind::NotFound) => ErrorKind::NotFound,
        FailureKind::Io(std::io::ErrorKind::PermissionDenied) => ErrorKind::Unauthorized,
        FailureKind::Io(std::io::ErrorKind::Unsupported) => ErrorKind::Unsupported,
        _ => ErrorKind::Network,
    };
    Error::new(kind, failure.message)
}

/// The `AVURLAsset` for a source. A URL with request headers loads through
/// our own loader, which the session must keep alive with the asset.
fn asset_for(source: &Source) -> Result<(Retained<AVURLAsset>, Option<loader::Loader>), Error> {
    let url = match source.location() {
        Location::File(path) => file_url(path)?,
        Location::Url(url) => web_url(url)?,
        _ => {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "the AVFoundation backend takes file paths and http(s) URLs",
            ));
        }
    };
    if !source.headers().is_empty() {
        if source.path().is_some() {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "request headers were given for a file, which makes no requests",
            ));
        }
        let url = source.as_url().unwrap_or_default();
        return loader::asset(url, source.headers())
            .map(|(asset, loader)| (asset, Some(loader)))
            .ok_or_else(|| Error::new(ErrorKind::Unsupported, format!("not a valid URL: {url}")));
    }
    // SAFETY: `url` is a valid NSURL, and there are no options.
    let asset = unsafe { AVURLAsset::initWithURL_options(AVURLAsset::alloc(), &url, None) };
    Ok((asset, None))
}

fn file_url(path: &Path) -> Result<Retained<NSURL>, Error> {
    let absolute = std::path::absolute(path).map_err(|error| {
        Error::new(
            ErrorKind::NotFound,
            format!("cannot resolve {}: {error}", path.display()),
        )
    })?;
    if !absolute.is_file() {
        return Err(Error::new(
            ErrorKind::NotFound,
            format!("no such file: {}", absolute.display()),
        ));
    }
    let path = absolute.to_str().ok_or_else(|| {
        Error::new(
            ErrorKind::Unsupported,
            format!("the path is not valid UTF-8: {}", absolute.display()),
        )
    })?;
    Ok(NSURL::fileURLWithPath(&NSString::from_str(path)))
}

fn web_url(url: &str) -> Result<Retained<NSURL>, Error> {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(Error::new(
            ErrorKind::Unsupported,
            format!("only http and https URLs are supported, not {url}"),
        ));
    }
    NSURL::URLWithString(&NSString::from_str(url))
        .ok_or_else(|| Error::new(ErrorKind::Unsupported, format!("not a valid URL: {url}")))
}
