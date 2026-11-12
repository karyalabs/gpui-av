//! Finding the HTTP status behind a failed load.
//!
//! AVFoundation reports some HTTP failures as damaged media: a 403 with an
//! empty body ends in "Cannot Open" (AVFoundationErrorDomain -11829) with no
//! status anywhere in the error chain. When an HTTP source fails that way,
//! the session asks the server once more, for two bytes with the same
//! headers, and reports by the status it gets.

use block2::RcBlock;
use gpui_av::{Error, ErrorKind, EventSink};
use objc2_foundation::{
    NSData, NSError, NSHTTPURLResponse, NSMutableURLRequest, NSString, NSURL, NSURLResponse,
    NSURLSession,
};

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
