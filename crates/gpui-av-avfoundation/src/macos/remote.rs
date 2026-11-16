//! Range requests through `NSURLSession`, for remuxing a Matroska or WebM
//! URL on the HLS server's thread. Each fetch blocks that thread (never
//! the main one) until the answer is in. The headers go to the URL's
//! origin only: a redirect elsewhere gets none (the session's delegate
//! sees each redirect, as the resource loader's does).

use std::io;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

use block2::{DynBlock, RcBlock};
use gpui_av_remux::{Fetched, RangeSource};
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

/// A URL and the headers that go with every request for it on its origin.
pub(crate) struct UrlRanges {
    url: String,
    headers: Vec<(String, String)>,
    session: Retained<NSURLSession>,
}

// SAFETY: NSURLSession is thread-safe; the session is only used to start
// tasks and, on drop, to be invalidated.
unsafe impl Send for UrlRanges {}

impl UrlRanges {
    /// `None` for a URL with no origin (not `http` or `https`).
    pub fn new(url: &str, headers: &[(String, String)]) -> Option<Self> {
        let parsed = NSURL::URLWithString(&NSString::from_str(url))?;
        let origin = Origin::of(&parsed)?;
        let delegate: Retained<Redirects> = {
            let this = Redirects::alloc().set_ivars(Policy {
                origin,
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
            url: url.to_string(),
            headers: headers.to_vec(),
            session,
        })
    }
}

impl Drop for UrlRanges {
    fn drop(&mut self) {
        // Releases the session's reference to its delegate.
        self.session.invalidateAndCancel();
    }
}

/// What a request's completion handler hands back.
type Answer = Result<(isize, Vec<u8>, Option<String>), String>;

impl RangeSource for UrlRanges {
    fn fetch(&mut self, start: u64, length: u64) -> io::Result<Fetched> {
        let url = NSURL::URLWithString(&NSString::from_str(&self.url))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a valid URL"))?;
        let request = NSMutableURLRequest::requestWithURL(&url);
        for (name, value) in &self.headers {
            request.setValue_forHTTPHeaderField(
                Some(&NSString::from_str(value)),
                &NSString::from_str(name),
            );
        }
        let range = format!("bytes={start}-{}", start + length.max(1) - 1);
        request.setValue_forHTTPHeaderField(
            Some(&NSString::from_str(&range)),
            &NSString::from_str("Range"),
        );
        let (sender, receiver) = mpsc::channel::<Answer>();
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
                            let status = http.map_or(200, |http| http.statusCode());
                            let content_range = http
                                .and_then(|http| {
                                    http.valueForHTTPHeaderField(&NSString::from_str(
                                        "Content-Range",
                                    ))
                                })
                                .map(|range| range.to_string());
                            Ok((
                                status,
                                data.map(NSData::to_vec).unwrap_or_default(),
                                content_range,
                            ))
                        }
                        _ => Err("no answer".to_string()),
                    }
                };
                if let Some(sender) = sender.lock().ok().and_then(|mut sender| sender.take())
                    && sender.send(answer).is_err()
                {
                    // The fetch gave up waiting; nothing to deliver to.
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
        let (status, bytes, content_range) =
            answer.map_err(|message| io::Error::new(io::ErrorKind::NotConnected, message))?;
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
