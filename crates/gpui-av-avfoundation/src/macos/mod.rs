//! AVFoundation playback. Everything Apple lives below this module.

mod errors;
mod info;
mod probe;
mod session;

use std::path::Path;

use gpui::App;
use gpui_av::{Backend, Error, ErrorKind, EventSink, Location, Session, Source};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AnyThread, MainThreadMarker};
use objc2_av_foundation::AVURLAsset;
use objc2_foundation::{NSDictionary, NSMutableDictionary, NSString, NSURL};

/// Plays media with AVFoundation's `AVPlayer` and draws it with GPUI's
/// surface. macOS only.
///
/// Sources: file paths, and `http`/`https` URLs with request headers. Every
/// header is sent with every request AVFoundation makes for the media,
/// range requests included.
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
        let asset = asset_for(&source)?;
        let http = source.as_url().map(|url| probe::HttpSource {
            url: url.to_string(),
            headers: source.headers().to_vec(),
        });
        Ok(Box::new(session::AvSession::new(
            asset, http, events, mtm, cx,
        )))
    }
}

/// The `AVURLAsset` for a source, with its request headers attached.
fn asset_for(source: &Source) -> Result<Retained<AVURLAsset>, Error> {
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
    let options = NSMutableDictionary::<NSString, AnyObject>::new();
    if !source.headers().is_empty() {
        if source.path().is_some() {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "request headers were given for a file, which makes no requests",
            ));
        }
        let names: Vec<Retained<NSString>> = source
            .headers()
            .iter()
            .map(|(name, _)| NSString::from_str(name))
            .collect();
        let values: Vec<Retained<NSString>> = source
            .headers()
            .iter()
            .map(|(_, value)| NSString::from_str(value))
            .collect();
        let names: Vec<&NSString> = names.iter().map(|name| &**name).collect();
        let values: Vec<&AnyObject> = values.iter().map(|value| value.as_ref()).collect();
        let headers = NSDictionary::from_slices(&names, &values);
        // SAFETY: the key is an NSString constant exported by AVFoundation,
        // and its value is an NSDictionary of NSString to NSString.
        unsafe {
            options.setObject_forKey(
                &headers,
                ProtocolObject::from_ref(ProtocolKey::http_header_fields()),
            );
        }
    }
    // SAFETY: `url` is a valid NSURL and `options` holds only keys
    // AVURLAsset documents (or exports), with values of the right types.
    let asset =
        unsafe { AVURLAsset::initWithURL_options(AVURLAsset::alloc(), &url, Some(&options)) };
    Ok(asset)
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

/// Option keys AVFoundation exports but the objc2 bindings do not declare.
struct ProtocolKey;

impl ProtocolKey {
    /// `AVURLAssetHTTPHeaderFieldsKey`: a dictionary of header fields that
    /// the asset sends with every HTTP request it makes. AVFoundation exports
    /// the symbol (it is in the SDK's `.tbd`), but no public header declares
    /// it, so objc2-av-foundation does not either.
    fn http_header_fields() -> &'static NSString {
        #[link(name = "AVFoundation", kind = "framework")]
        unsafe extern "C" {
            static AVURLAssetHTTPHeaderFieldsKey: &'static NSString;
        }
        // SAFETY: the symbol is an immutable NSString constant that lives
        // as long as the framework, which is never unloaded.
        unsafe { AVURLAssetHTTPHeaderFieldsKey }
    }
}
