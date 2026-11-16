//! Our own HTTP loading for sources with request headers (spec R6).
//!
//! AVFoundation sends custom headers only through an option no public
//! header declares, and it forwards them to the target of a cross-origin
//! redirect. Instead, an asset for a source with headers gets a made-up URL
//! scheme, so AVFoundation hands every byte range it wants to an
//! `AVAssetResourceLoaderDelegate`: this module. It fetches each range with
//! `NSURLSession`, with the source's headers, and streams the bytes back.
//! Redirects keep the headers only while they stay on the source's origin.
//!
//! Every callback, AVFoundation's and NSURLSession's, runs on one serial
//! dispatch queue, so the state here is touched by one thread at a time.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use block2::DynBlock;
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_av_foundation::{
    AVAssetResourceLoader, AVAssetResourceLoaderDelegate, AVAssetResourceLoadingRequest, AVURLAsset,
};
use objc2_foundation::{
    NSData, NSDictionary, NSError, NSHTTPURLResponse, NSInteger, NSLocalizedDescriptionKey,
    NSMutableURLRequest, NSObjectProtocol, NSOperationQueue, NSString, NSURL, NSURLRequest,
    NSURLResponse, NSURLSession, NSURLSessionConfiguration, NSURLSessionDataDelegate,
    NSURLSessionDataTask, NSURLSessionDelegate, NSURLSessionResponseDisposition, NSURLSessionTask,
    NSURLSessionTaskDelegate,
};
use objc2_uniform_type_identifiers::UTType;

/// The error domain of the loader's own failures.
pub(crate) const ERROR_DOMAIN: &str = "GpuiAvHttpLoader";
/// AVFoundation passes on only the code of a delegate's error, cut to
/// 16 bits, as an OSStatus. So the codes say everything, and stay in a
/// 16-bit range no system OSStatus uses: [`CODE_BASE`] minus the HTTP
/// status for a status that ended a load, or minus [`UNSUPPORTED`] for a
/// source the loader cannot play (an HLS playlist, a server without ranges).
pub(crate) const CODE_BASE: NSInteger = -31_000;
/// Taken from [`CODE_BASE`] for a source the loader cannot play.
pub(crate) const UNSUPPORTED: NSInteger = 1;

/// The code for an HTTP status.
fn http_code(status: NSInteger) -> NSInteger {
    CODE_BASE - status
}

/// What a loader error code means: `Some(Ok(status))` for an HTTP status,
/// `Some(Err(()))` for unsupported, `None` for a code that is not ours.
pub(crate) fn decode_code(code: isize) -> Option<Result<u16, ()>> {
    let offset = CODE_BASE.checked_sub(code)?;
    match offset {
        UNSUPPORTED => Some(Err(())),
        100..=599 => Some(Ok(offset as u16)),
        _ => None,
    }
}

/// The made-up scheme for `scheme`: AVFoundation loads `http` and `https`
/// itself, and asks its delegate only for schemes it does not know.
const PREFIX: &str = "gpuiav-";

/// The real URL's scheme, host and port: headers go only where these match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Origin {
    scheme: String,
    host: String,
    port: Option<u16>,
}

impl Origin {
    pub(crate) fn of(url: &NSURL) -> Option<Self> {
        let scheme = url.scheme()?.to_string().to_ascii_lowercase();
        let host = url.host()?.to_string().to_ascii_lowercase();
        let port = url
            .port()
            .and_then(|port| u16::try_from(port.integerValue()).ok())
            .or(match scheme.as_str() {
                "http" => Some(80),
                "https" => Some(443),
                _ => None,
            });
        Some(Self { scheme, host, port })
    }
}

/// An `AVAssetResourceLoadingRequest`, used only on the loader's queue.
struct Loading(Retained<AVAssetResourceLoadingRequest>);
// SAFETY: every use of the request is on the one serial queue that
// AVFoundation calls the delegate on.
unsafe impl Send for Loading {}

struct State {
    url: String,
    origin: Origin,
    headers: Vec<(String, String)>,
    /// The loading request each running data task serves, by task id.
    pending: Mutex<HashMap<usize, Loading>>,
    /// The session the data tasks run in. It retains the delegate, which
    /// holds this state: `Loader::drop` breaks the cycle.
    session: Mutex<Option<SessionPtr>>,
}

/// Keeps the loader alive for as long as its asset: AVFoundation holds the
/// delegate weakly. Dropping it cancels every request in flight.
pub(crate) struct Loader {
    delegate: Retained<HttpLoader>,
    session: Retained<NSURLSession>,
    _queue: DispatchRetained<DispatchQueue>,
}

impl Drop for Loader {
    fn drop(&mut self) {
        // Cancels every request, and releases the session's reference to
        // the delegate.
        self.session.invalidateAndCancel();
        self.delegate
            .ivars()
            .session
            .lock()
            .expect("loader state")
            .take();
    }
}

define_class!(
    /// The resource loader's delegate and the URL session's delegate.
    #[unsafe(super(NSObject))]
    #[name = "GpuiAvHttpLoader"]
    #[ivars = Arc<State>]
    struct HttpLoader;

    unsafe impl NSObjectProtocol for HttpLoader {}

    unsafe impl AVAssetResourceLoaderDelegate for HttpLoader {
        #[unsafe(method(resourceLoader:shouldWaitForLoadingOfRequestedResource:))]
        fn should_wait(
            &self,
            _loader: &AVAssetResourceLoader,
            loading: &AVAssetResourceLoadingRequest,
        ) -> bool {
            self.start(loading);
            true
        }

        #[unsafe(method(resourceLoader:didCancelLoadingRequest:))]
        fn did_cancel(&self, _loader: &AVAssetResourceLoader, loading: &AVAssetResourceLoadingRequest) {
            let task = {
                let mut pending = self.ivars().pending.lock().expect("loader state");
                let id = pending
                    .iter()
                    .find(|(_, request)| std::ptr::eq(&*request.0, loading))
                    .map(|(id, _)| *id);
                id.filter(|id| pending.remove(id).is_some())
            };
            if let Some(id) = task {
                self.cancel_task(id);
            }
        }
    }

    unsafe impl NSURLSessionDelegate for HttpLoader {}

    unsafe impl NSURLSessionTaskDelegate for HttpLoader {
        #[unsafe(method(URLSession:task:willPerformHTTPRedirection:newRequest:completionHandler:))]
        fn will_redirect(
            &self,
            _session: &NSURLSession,
            _task: &NSURLSessionTask,
            _response: &NSHTTPURLResponse,
            request: &NSURLRequest,
            completion: &DynBlock<dyn Fn(*mut NSURLRequest)>,
        ) {
            let next = self.redirected(request);
            completion.call((Retained::as_ptr(&next) as *mut NSURLRequest,));
        }

        #[unsafe(method(URLSession:task:didCompleteWithError:))]
        fn did_complete(&self, _session: &NSURLSession, task: &NSURLSessionTask, error: Option<&NSError>) {
            let id = task.taskIdentifier();
            let Some(loading) = self.ivars().pending.lock().expect("loader state").remove(&id) else {
                return;
            };
            // SAFETY: loading-request calls on the loader's queue.
            unsafe {
                if loading.0.isFinished() || loading.0.isCancelled() {
                    return;
                }
                match error {
                    None => loading.0.finishLoading(),
                    Some(error) => loading.0.finishLoadingWithError(Some(error)),
                }
            }
        }
    }

    unsafe impl NSURLSessionDataDelegate for HttpLoader {
        #[unsafe(method(URLSession:dataTask:didReceiveResponse:completionHandler:))]
        fn did_receive_response(
            &self,
            _session: &NSURLSession,
            task: &NSURLSessionDataTask,
            response: &NSURLResponse,
            completion: &DynBlock<dyn Fn(NSURLSessionResponseDisposition)>,
        ) {
            let carry_on = self.on_response(task.taskIdentifier(), response);
            completion.call((if carry_on {
                NSURLSessionResponseDisposition::Allow
            } else {
                NSURLSessionResponseDisposition::Cancel
            },));
        }

        #[unsafe(method(URLSession:dataTask:didReceiveData:))]
        fn did_receive_data(&self, _session: &NSURLSession, task: &NSURLSessionDataTask, data: &NSData) {
            let pending = self.ivars().pending.lock().expect("loader state");
            if let Some(loading) = pending.get(&task.taskIdentifier())
                // SAFETY: loading-request calls on the loader's queue.
                && let Some(request) = unsafe { loading.0.dataRequest() }
            {
                // SAFETY: as above.
                unsafe { request.respondWithData(data) };
            }
        }
    }
);

impl HttpLoader {
    /// Starts fetching the bytes `loading` asks for.
    fn start(&self, loading: &AVAssetResourceLoadingRequest) {
        let state = self.ivars();
        let Some(url) = NSURL::URLWithString(&NSString::from_str(&state.url)) else {
            fail(loading, CODE_BASE - UNSUPPORTED, "not a valid URL");
            return;
        };
        let request = NSMutableURLRequest::requestWithURL(&url);
        for (name, value) in &state.headers {
            request.setValue_forHTTPHeaderField(
                Some(&NSString::from_str(value)),
                &NSString::from_str(name),
            );
        }
        // SAFETY: loading-request reads on the loader's queue.
        let range = unsafe {
            match loading.dataRequest() {
                Some(data) => {
                    let offset = data.requestedOffset().max(0);
                    if data.requestsAllDataToEndOfResource() {
                        format!("bytes={offset}-")
                    } else {
                        let length = (data.requestedLength() as i64).max(1);
                        format!("bytes={offset}-{}", offset + length - 1)
                    }
                }
                // Only the content information: two bytes tell everything.
                None => "bytes=0-1".to_string(),
            }
        };
        request.setValue_forHTTPHeaderField(
            Some(&NSString::from_str(&range)),
            &NSString::from_str("Range"),
        );
        let Some(session) = self.session() else {
            fail(loading, CODE_BASE - UNSUPPORTED, "the loader is closed");
            return;
        };
        let task = session.dataTaskWithRequest(&request);
        state
            .pending
            .lock()
            .expect("loader state")
            .insert(task.taskIdentifier(), Loading(loading.retain()));
        task.resume();
    }

    fn session(&self) -> Option<Retained<NSURLSession>> {
        let session = self.ivars().session.lock().expect("loader state");
        session.as_ref().map(|session| session.0.clone())
    }

    fn cancel_task(&self, id: usize) {
        let Some(session) = self.session() else {
            return;
        };
        let block = block2::RcBlock::new(
            move |tasks: std::ptr::NonNull<objc2_foundation::NSArray<NSURLSessionTask>>| {
                // SAFETY: the array is live for the block's duration.
                for task in unsafe { tasks.as_ref() }.iter() {
                    if task.taskIdentifier() == id {
                        task.cancel();
                    }
                }
            },
        );
        // SAFETY: the block takes the task array, and only uses Send values.
        unsafe { session.getAllTasksWithCompletionHandler(&block) };
    }

    fn redirected(&self, request: &NSURLRequest) -> Retained<NSURLRequest> {
        let state = self.ivars();
        redirected(request, &state.origin, &state.headers)
    }

    /// Fills in what AVFoundation asked about the resource, or fails the
    /// request. Returns whether the body should keep coming.
    fn on_response(&self, id: usize, response: &NSURLResponse) -> bool {
        let pending = self.ivars().pending.lock().expect("loader state");
        let Some(loading) = pending.get(&id) else {
            return false;
        };
        let loading = &loading.0;
        let http = response.downcast_ref::<NSHTTPURLResponse>();
        let status = http.map_or(200, |http| http.statusCode());
        if !(200..300).contains(&status) {
            fail(loading, http_code(status), &format!("HTTP {status}"));
            return false;
        }
        let header = |name: &str| {
            http.and_then(|http| http.valueForHTTPHeaderField(&NSString::from_str(name)))
                .map(|value| value.to_string())
        };
        let mime = response
            .MIMEType()
            .map(|mime| mime.to_string())
            .unwrap_or_default();
        if is_playlist(&mime) {
            fail(
                loading,
                CODE_BASE - UNSUPPORTED,
                "playlists (HLS) with request headers are not supported yet",
            );
            return false;
        }
        // SAFETY: loading-request calls on the loader's queue.
        unsafe {
            let asked_from = loading
                .dataRequest()
                .map_or(0, |data| data.requestedOffset());
            if status == 200 && asked_from > 0 {
                fail(
                    loading,
                    CODE_BASE - UNSUPPORTED,
                    "the server ignores range requests, which seeking needs",
                );
                return false;
            }
            if let Some(info) = loading.contentInformationRequest() {
                let content_type = uti(&mime, &self.ivars().url);
                info.setContentType(Some(&NSString::from_str(&content_type)));
                let total = header("Content-Range")
                    .and_then(|range| total_length(&range))
                    .or((status == 200)
                        .then(|| response.expectedContentLength())
                        .filter(|length| *length >= 0));
                if let Some(total) = total {
                    info.setContentLength(total);
                }
                info.setByteRangeAccessSupported(
                    status == 206
                        || header("Accept-Ranges").is_some_and(|ranges| ranges.contains("bytes")),
                );
                if loading.dataRequest().is_none() {
                    loading.finishLoading();
                    return false;
                }
            }
        }
        true
    }
}

/// The request to follow a redirect with. NSURLSession drops
/// `Authorization` on every redirect but keeps every other header, so the
/// source's headers are put back on the source's origin, and removed (all of
/// them) anywhere else. `Range` stays: it is the request itself.
pub(crate) fn redirected(
    request: &NSURLRequest,
    origin: &Origin,
    headers: &[(String, String)],
) -> Retained<NSURLRequest> {
    let same_origin = request
        .URL()
        .and_then(|url| Origin::of(&url))
        .is_some_and(|to| &to == origin);
    // SAFETY: mutableCopy of an NSURLRequest is an NSMutableURLRequest.
    let next: Retained<NSMutableURLRequest> = unsafe { msg_send![request, mutableCopy] };
    for (name, value) in headers {
        let value = same_origin.then(|| NSString::from_str(value));
        next.setValue_forHTTPHeaderField(value.as_deref(), &NSString::from_str(name));
    }
    Retained::into_super(next)
}

struct SessionPtr(Retained<NSURLSession>);
// SAFETY: NSURLSession is thread-safe.
unsafe impl Send for SessionPtr {}

fn fail(loading: &AVAssetResourceLoadingRequest, code: NSInteger, message: &str) {
    let description = NSString::from_str(message);
    // SAFETY: a constant NSString key.
    let key: &NSString = unsafe { NSLocalizedDescriptionKey };
    let info = NSDictionary::<NSString, AnyObject>::from_slices(&[key], &[&*description]);
    // SAFETY: a domain, a code, and a dictionary of NSString to NSString.
    let error = unsafe {
        NSError::errorWithDomain_code_userInfo(&NSString::from_str(ERROR_DOMAIN), code, Some(&info))
    };
    // SAFETY: a loading-request call on the loader's queue.
    unsafe {
        if !loading.isFinished() && !loading.isCancelled() {
            loading.finishLoadingWithError(Some(&error));
        }
    }
}

fn is_playlist(mime: &str) -> bool {
    let mime = mime.to_ascii_lowercase();
    mime.contains("mpegurl") || mime == "application/dash+xml"
}

/// The total length in `Content-Range: bytes 0-1/12345`.
fn total_length(content_range: &str) -> Option<i64> {
    content_range.rsplit('/').next()?.trim().parse().ok()
}

/// The uniform type AVFoundation wants for the content: from the server's
/// MIME type when it names a media type, else from the URL's extension.
fn uti(mime: &str, url: &str) -> String {
    let from_mime = (!mime.is_empty() && mime != "application/octet-stream")
        .then(|| UTType::typeWithMIMEType(&NSString::from_str(mime)))
        .flatten();
    let from_extension = || {
        let path = url.split(['?', '#']).next().unwrap_or(url);
        let extension = path.rsplit_once('.').map(|(_, extension)| extension)?;
        UTType::typeWithFilenameExtension(&NSString::from_str(extension))
    };
    from_mime.or_else(from_extension).map_or_else(
        || "public.mpeg-4".to_string(),
        |kind| kind.identifier().to_string(),
    )
}

/// An asset that loads `url` through our own requests, carrying `headers`.
/// The returned loader must live as long as the asset.
pub(crate) fn asset(
    url: &str,
    headers: &[(String, String)],
) -> Option<(Retained<AVURLAsset>, Loader)> {
    let real = NSURL::URLWithString(&NSString::from_str(url))?;
    let origin = Origin::of(&real)?;
    if origin.scheme != "http" && origin.scheme != "https" {
        return None;
    }
    let made_up = format!("{PREFIX}{url}");
    let made_up = NSURL::URLWithString(&NSString::from_str(&made_up))?;
    let state = Arc::new(State {
        url: url.to_string(),
        origin,
        headers: headers.to_vec(),
        pending: Mutex::new(HashMap::new()),
        session: Mutex::new(None),
    });
    let queue = DispatchQueue::new("gpui-av.http-loader", DispatchQueueAttr::SERIAL);
    let delegate: Retained<HttpLoader> = {
        let this = HttpLoader::alloc().set_ivars(state);
        // SAFETY: plain NSObject initialisation.
        unsafe { msg_send![super(this), init] }
    };
    // The session's callbacks run on the same serial queue as the loader's.
    let operations = NSOperationQueue::new();
    operations.setMaxConcurrentOperationCount(1);
    // SAFETY: -[NSOperationQueue setUnderlyingQueue:] takes a dispatch
    // queue, which is an Objective-C object; the queue outlives the session.
    unsafe {
        let underlying = (&*queue as *const DispatchQueue).cast::<AnyObject>();
        let _: () = msg_send![&operations, setUnderlyingQueue: underlying];
    }
    let configuration = NSURLSessionConfiguration::ephemeralSessionConfiguration();
    // SAFETY: the delegate conforms to the session delegate protocols.
    let session = unsafe {
        NSURLSession::sessionWithConfiguration_delegate_delegateQueue(
            &configuration,
            Some(ProtocolObject::from_ref(&*delegate)),
            Some(&operations),
        )
    };
    *delegate.ivars().session.lock().expect("loader state") = Some(SessionPtr(session.clone()));
    // SAFETY: a URL of a scheme AVFoundation does not load itself, and no
    // options.
    let asset = unsafe { AVURLAsset::initWithURL_options(AVURLAsset::alloc(), &made_up, None) };
    // SAFETY: the delegate conforms to the protocol, and the queue is a
    // serial dispatch queue that outlives the asset (the loader keeps it).
    unsafe {
        asset
            .resourceLoader()
            .setDelegate_queue(Some(ProtocolObject::from_ref(&*delegate)), Some(&queue));
    }
    Some((
        asset,
        Loader {
            delegate,
            session,
            _queue: queue,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_total_length() {
        assert_eq!(total_length("bytes 0-1/72172630"), Some(72_172_630));
        assert_eq!(total_length("bytes */100"), Some(100));
        assert_eq!(total_length("garbage"), None);
    }

    #[test]
    fn codes_survive_a_cut_to_16_bits() {
        for status in [401, 403, 404, 500] {
            let cut = http_code(status) as i16 as isize;
            assert_eq!(decode_code(cut), Some(Ok(status as u16)));
        }
        let cut = (CODE_BASE - UNSUPPORTED) as i16 as isize;
        assert_eq!(decode_code(cut), Some(Err(())));
        assert_eq!(decode_code(-12_848), None);
        assert_eq!(decode_code(404), None);
    }

    #[test]
    fn knows_a_playlist() {
        assert!(is_playlist("application/vnd.apple.mpegurl"));
        assert!(is_playlist("audio/mpegURL"));
        assert!(!is_playlist("video/mp4"));
    }
}
