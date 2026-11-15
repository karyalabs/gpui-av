//! AVFoundation playback. Everything Apple lives below this module.

mod errors;
mod info;
mod loader;
mod probe;
mod session;
mod tracks;

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
        let (asset, loader) = asset_for(&source)?;
        let http = source.as_url().map(|url| probe::HttpSource {
            url: url.to_string(),
            headers: source.headers().to_vec(),
        });
        Ok(Box::new(session::AvSession::new(
            asset, loader, http, events, mtm, cx,
        )))
    }
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
