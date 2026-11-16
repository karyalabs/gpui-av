//! Range requests through `NSURLSession`, for remuxing a Matroska or WebM
//! URL on the HLS server's thread. Each fetch blocks that thread (never
//! the main one) until the answer is in; the HLS proxy's requests (HLS
//! with request headers) go through the same session. The headers go to the URL's
//! origin only: a redirect elsewhere gets none (the session's delegate
//! sees each redirect, as the resource loader's does).

use std::io;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

use block2::{DynBlock, RcBlock};
use gpui_av_remux::{Fetch, Fetched, HttpResponse, RangeSource};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, ProtocolObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::{
    NSData, NSError, NSHTTPURLResponse, NSMutableURLRequest, NSObjectProtocol, NSString, NSURL,
    NSURLRequest, NSURLResponse, NSURLSession, NSURLSessionConfiguration, NSURLSessionDelegate,
    NSURLSessionTask, NSURLSessionTaskDelegate,
};

use super::loader::{Origin, redirected};

/// How long one range request may take.
const TIMEOUT: Duration = Duration::from_secs(60);

struct Policy {
    origin: Origin,
    headers: Vec<(String, String)>,
}

define_class!(
    /// Applies the origin rule to each redirect of the remuxer's requests.
    #[unsafe(super(NSObject))]
    #[name = "GpuiAvRangeRedirects"]
    #[ivars = Policy]
    struct Redirects;

    unsafe impl NSObjectProtocol for Redirects {}
    unsafe impl NSURLSessionDelegate for Redirects {}

    unsafe impl NSURLSessionTaskDelegate for Redirects {
        #[unsafe(method(URLSession:task:willPerformHTTPRedirection:newRequest:completionHandler:))]
        fn will_redirect(
            &self,
            _session: &NSURLSession,
            _task: &NSURLSessionTask,
            _response: &NSHTTPURLResponse,
            request: &NSURLRequest,
            completion: &DynBlock<dyn Fn(*mut NSURLRequest)>,
        ) {
            let policy = self.ivars();
            let next = redirected(request, &policy.origin, &policy.headers);
            completion.call((Retained::as_ptr(&next) as *mut NSURLRequest,));
        }
    }
);

/// A session for one source: requests carry the source's headers on its
/// origin only, redirects included.
pub(crate) struct Requests {
    origin: Origin,
    headers: Vec<(String, String)>,
    session: Retained<NSURLSession>,
}

// SAFETY: NSURLSession is thread-safe; the session is only used to start
// tasks and, on drop, to be invalidated.
unsafe impl Send for Requests {}
// SAFETY: as above; nothing in it changes after it is made.
unsafe impl Sync for Requests {}

impl Requests {
    /// `None` for a URL with no origin (not `http` or `https`).
    pub fn new(url: &str, headers: &[(String, String)]) -> Option<Self> {
        let parsed = NSURL::URLWithString(&NSString::from_str(url))?;
        let origin = Origin::of(&parsed)?;
        let delegate: Retained<Redirects> = {
            let this = Redirects::alloc().set_ivars(Policy {
                origin: origin.clone(),
                headers: headers.to_vec(),
            });
            // SAFETY: plain NSObject initialisation.
            unsafe { msg_send![super(this), init] }
        };
        let configuration = NSURLSessionConfiguration::ephemeralSessionConfiguration();
        // SAFETY: the delegate conforms to the session delegate protocols;
        // the session retains it until invalidated, and runs its callbacks
        // on a queue of its own.
        let session = unsafe {
            NSURLSession::sessionWithConfiguration_delegate_delegateQueue(
                &configuration,
                Some(ProtocolObject::from_ref(&*delegate)),
                None,
            )
        };
        Some(Self {
            origin,
            headers: headers.to_vec(),
            session,
        })
    }

    /// GETs `url`, bytes `start` to `end` when a range is given, blocking
    /// until the answer is in.
    pub fn get(&self, url: &str, range: Option<(u64, Option<u64>)>) -> io::Result<HttpResponse> {
        let parsed = NSURL::URLWithString(&NSString::from_str(url))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a valid URL"))?;
        let request = NSMutableURLRequest::requestWithURL(&parsed);
        if Origin::of(&parsed).is_some_and(|origin| origin == self.origin) {
            for (name, value) in &self.headers {
                request.setValue_forHTTPHeaderField(
                    Some(&NSString::from_str(value)),
                    &NSString::from_str(name),
                );
            }
        }
        if let Some((start, end)) = range {
            let end = end.map(|end| end.to_string()).unwrap_or_default();
            request.setValue_forHTTPHeaderField(
                Some(&NSString::from_str(&format!("bytes={start}-{end}"))),
                &NSString::from_str("Range"),
            );
        }
        let (sender, receiver) = mpsc::channel::<Result<HttpResponse, String>>();
        // The handler is called once; the sender is taken out on that call.
        let sender = Mutex::new(Some(sender));
        let handler = RcBlock::new(
            move |data: *mut NSData, response: *mut NSURLResponse, error: *mut NSError| {
                // SAFETY: the pointers are null or live objects for the
                // duration of the call.
                let answer = unsafe {
                    match (data.as_ref(), response.as_ref(), error.as_ref()) {
                        (_, _, Some(error)) => Err(error.localizedDescription().to_string()),
                        (data, Some(response), None) => {
                            let http = response.downcast_ref::<NSHTTPURLResponse>();
                            let header = |name: &str| {
                                http.and_then(|http| {
                                    http.valueForHTTPHeaderField(&NSString::from_str(name))
                                })
                                .map(|value| value.to_string())
                            };
                            Ok(HttpResponse {
                                status: http.map_or(200, |http| http.statusCode()) as u16,
                                content_type: header("Content-Type"),
                                content_range: header("Content-Range"),
                                url: response
                                    .URL()
                                    .and_then(|url| url.absoluteString())
                                    .map(|url| url.to_string())
                                    .unwrap_or_default(),
                                body: data.map(NSData::to_vec).unwrap_or_default(),
                            })
                        }
                        _ => Err("no answer".to_string()),
                    }
                };
                if let Some(sender) = sender.lock().ok().and_then(|mut sender| sender.take())
                    && sender.send(answer).is_err()
                {
                    // The request gave up waiting; nothing to deliver to.
                }
            },
        );
        // SAFETY: the session's tasks call the handler on its delegate
        // queue; everything it captures is Send. With a completion
        // handler the delegate still sees redirects.
        let task = unsafe {
            self.session
                .dataTaskWithRequest_completionHandler(&request, &handler)
        };
        task.resume();
        let answer = receiver.recv_timeout(TIMEOUT).map_err(|_| {
            task.cancel();
            io::Error::new(io::ErrorKind::TimedOut, "the server did not answer in time")
        })?;
        answer.map_err(|message| io::Error::new(io::ErrorKind::NotConnected, message))
    }
}

impl Drop for Requests {
    fn drop(&mut self) {
        // Releases the session's reference to its delegate.
        self.session.invalidateAndCancel();
    }
}

/// HLS through the loopback proxy: every playlist, key and segment with
/// the source's headers.
impl Fetch for Requests {
    fn get(&self, url: &str, range: Option<(u64, Option<u64>)>) -> io::Result<HttpResponse> {
        Requests::get(self, url, range)
    }
}

/// Byte ranges of one URL, for the remuxer.
pub(crate) struct UrlRanges {
    url: String,
    requests: Requests,
}

impl UrlRanges {
    /// `None` for a URL with no origin (not `http` or `https`).
    pub fn new(url: &str, headers: &[(String, String)]) -> Option<Self> {
        Some(Self {
            url: url.to_string(),
            requests: Requests::new(url, headers)?,
        })
    }
}

impl RangeSource for UrlRanges {
    fn fetch(&mut self, start: u64, length: u64) -> io::Result<Fetched> {
        let end = start + length.max(1) - 1;
        let HttpResponse {
            status,
            content_range,
            body: bytes,
            ..
        } = self.requests.get(&self.url, Some((start, Some(end))))?;
        match status {
            206 => {}
            // The whole file, from a server that ignores ranges: only good
            // when it fits in what was asked for.
            200 if start == 0 && bytes.len() as u64 <= length => {}
            200 => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "the server does not take range requests, which remuxing needs",
                ));
            }
            404 | 410 => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("the server answered {status}"),
                ));
            }
            401 | 403 => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("the server answered {status}"),
                ));
            }
            // Asked for bytes past the end.
            416 => {
                return Ok(Fetched {
                    bytes: Vec::new(),
                    length: None,
                });
            }
            _ => return Err(io::Error::other(format!("the server answered {status}"))),
        }
        let length = match status {
            200 => Some(bytes.len() as u64),
            _ => content_range.and_then(|range| {
                range
                    .rsplit_once('/')
                    .and_then(|(_, total)| total.parse().ok())
            }),
        };
        Ok(Fetched { bytes, length })
    }
}
