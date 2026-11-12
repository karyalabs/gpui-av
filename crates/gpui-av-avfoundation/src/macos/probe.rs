//! Finding the HTTP status behind a failed load.
//!
//! AVFoundation reports some HTTP failures as damaged media: a 403 with an
//! empty body ends in "Cannot Open" (AVFoundationErrorDomain -11829) with no
//! status anywhere in the error chain. When an HTTP source fails that way,
//! the session asks the server once more, for two bytes with the same
//! headers, and reports by the status it gets.

use block2::{DynBlock, RcBlock};
use gpui_av::{Error, ErrorKind, EventSink};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, ProtocolObject};
use objc2::{AnyThread, define_class, msg_send};
use objc2_foundation::{
    NSData, NSError, NSHTTPURLResponse, NSMutableURLRequest, NSObjectProtocol, NSString, NSURL,
    NSURLRequest, NSURLResponse, NSURLSession, NSURLSessionDelegate, NSURLSessionTask,
    NSURLSessionTaskDelegate,
};

define_class!(
    /// Refuses every redirect, so the probe's headers (credentials, often)
    /// only ever go to the URL the app gave. The probe then sees the 3xx
    /// itself, which explains nothing, and the original error stands.
    #[unsafe(super(NSObject))]
    #[name = "GpuiAvProbeNoRedirects"]
    struct NoRedirects;

    unsafe impl NSObjectProtocol for NoRedirects {}
    unsafe impl NSURLSessionDelegate for NoRedirects {}
    unsafe impl NSURLSessionTaskDelegate for NoRedirects {
        #[unsafe(method(URLSession:task:willPerformHTTPRedirection:newRequest:completionHandler:))]
        fn refuse_redirect(
            &self,
            _session: &NSURLSession,
            _task: &NSURLSessionTask,
            _response: &NSHTTPURLResponse,
            _request: &NSURLRequest,
            completion_handler: &DynBlock<dyn Fn(*mut NSURLRequest)>,
        ) {
            completion_handler.call((std::ptr::null_mut(),));
        }
    }
);

impl NoRedirects {
    fn new() -> Retained<Self> {
        // SAFETY: plain NSObject initialisation of a class with no ivars.
        unsafe { msg_send![Self::alloc(), init] }
    }
}

/// The HTTP source a session plays, kept for the probe.
#[derive(Clone)]
pub(crate) struct HttpSource {
    pub url: String,
    pub headers: Vec<(String, String)>,
}

/// Whether an error from AVFoundation might hide an HTTP status.
pub(crate) fn worth_probing(error: &Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::Decode | ErrorKind::Unsupported | ErrorKind::Other
    )
}

/// Reports the failure through `events` once the server has answered:
/// by its status when that explains the failure, otherwise as `fallback`.
pub(crate) fn report(source: &HttpSource, fallback: Error, events: EventSink) {
    let Some(url) = NSURL::URLWithString(&NSString::from_str(&source.url)) else {
        events.fail(fallback);
        return;
    };
    let request = NSMutableURLRequest::requestWithURL(&url);
    for (name, value) in &source.headers {
        request.setValue_forHTTPHeaderField(
            Some(&NSString::from_str(value)),
            &NSString::from_str(name),
        );
    }
    request.setValue_forHTTPHeaderField(
        Some(&NSString::from_str("bytes=0-1")),
        &NSString::from_str("Range"),
    );
    let done = RcBlock::new(
        move |_: *mut NSData, response: *mut NSURLResponse, _: *mut NSError| {
            // SAFETY: the response is null or live for the block's duration.
            let status = unsafe { response.as_ref() }
                .and_then(|response| response.downcast_ref::<NSHTTPURLResponse>())
                .map(NSHTTPURLResponse::statusCode);
            events.fail(classify(status, fallback.clone()));
        },
    );
    // SAFETY: the block only holds Send values (an EventSink and an Error),
    // as NSURLSession calls it on its own queue.
    let task = unsafe {
        NSURLSession::sharedSession().dataTaskWithRequest_completionHandler(&request, &done)
    };
    // The task keeps its delegate until it finishes.
    task.setDelegate(Some(ProtocolObject::from_ref(&*NoRedirects::new())));
    task.resume();
}

fn classify(status: Option<isize>, fallback: Error) -> Error {
    let kind = match status {
        Some(401 | 403) => ErrorKind::Unauthorized,
        Some(404 | 410) => ErrorKind::NotFound,
        Some(status) if status >= 400 => ErrorKind::Network,
        _ => return fallback,
    };
    let status = status.unwrap_or_default();
    Error::new(kind, format!("HTTP {status} ({fallback})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_forbidden_status_explains_a_failed_load() {
        let fallback = Error::new(ErrorKind::Decode, "Cannot Open");
        assert_eq!(
            classify(Some(403), fallback.clone()).kind(),
            ErrorKind::Unauthorized
        );
        assert_eq!(
            classify(Some(500), fallback.clone()).kind(),
            ErrorKind::Network
        );
        assert_eq!(classify(Some(206), fallback.clone()), fallback);
        assert_eq!(classify(None, fallback.clone()), fallback);
    }
}
