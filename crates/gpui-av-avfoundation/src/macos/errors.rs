//! Turning `NSError`s from AVFoundation into gpui-av errors.

use gpui_av::{Error, ErrorKind};

use super::loader;
use objc2::Message;
use objc2::rc::Retained;
use objc2_av_foundation::{AVError, AVFoundationErrorDomain};
use objc2_foundation::{NSError, NSString, NSUnderlyingErrorKey};

/// One error in the chain: the error and its underlying errors.
struct Link {
    domain: String,
    code: isize,
    text: String,
}

/// The kind and message for an `NSError` from AVFoundation, reading its
/// whole chain of underlying errors.
pub(crate) fn from_ns_error(error: &NSError) -> Error {
    let chain = chain(error);
    let kind = classify(&chain);
    let message = chain
        .iter()
        .map(|link| format!("{} ({} {})", link.text, link.domain, link.code))
        .collect::<Vec<_>>()
        .join(": ");
    Error::new(kind, message)
}

fn chain(error: &NSError) -> Vec<Link> {
    let mut links = Vec::new();
    let mut next: Option<Retained<NSError>> = Some(error.retain());
    while let Some(error) = next.take() {
        let mut text = error.localizedDescription().to_string();
        if let Some(reason) = error.localizedFailureReason() {
            text.push_str(". ");
            text.push_str(&reason.to_string());
        }
        links.push(Link {
            domain: error.domain().to_string(),
            code: error.code(),
            text,
        });
        if links.len() >= 8 {
            break;
        }
        // SAFETY: NSUnderlyingErrorKey is a constant NSString key.
        let key: &NSString = unsafe { NSUnderlyingErrorKey };
        next = error
            .userInfo()
            .objectForKey(key)
            .and_then(|underlying| underlying.downcast::<NSError>().ok());
    }
    links
}

/// The status a CoreMedia HTTP error names in its description
/// ("HTTP 403: Forbidden"). CoreMedia's codes for these are not public, so
/// the description is the only stable signal.
fn http_status(text: &str) -> Option<u16> {
    let rest = &text[text.find("HTTP ")? + 5..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn classify(chain: &[Link]) -> ErrorKind {
    // The most specific cause wins, wherever it is in the chain.
    for link in chain {
        match http_status(&link.text) {
            Some(401 | 403) => return ErrorKind::Unauthorized,
            Some(404 | 410) => return ErrorKind::NotFound,
            _ => {}
        }
        match (link.domain.as_str(), link.code) {
            // Our own loader's statuses and refusals, whatever domain
            // AVFoundation wrapped their code in.
            (_, code) if loader::decode_code(code).is_some() => {
                return match loader::decode_code(code) {
                    Some(Ok(401 | 403)) => ErrorKind::Unauthorized,
                    Some(Ok(404 | 410)) => ErrorKind::NotFound,
                    Some(Ok(_)) => ErrorKind::Network,
                    _ => ErrorKind::Unsupported,
                };
            }
            // NSURLErrorUserAuthenticationRequired, NSURLErrorUserCancelledAuthentication,
            // NSURLErrorNoPermissionsToReadFile (an HTTP 403 after a redirect)
            ("NSURLErrorDomain", -1013 | -1012 | -1102) => return ErrorKind::Unauthorized,
            // CoreMedia's HTTP 403 and 404, seen as OSStatus codes.
            ("NSOSStatusErrorDomain", -12660) => return ErrorKind::Unauthorized,
            ("NSOSStatusErrorDomain", -12938) => return ErrorKind::NotFound,
            // NSURLErrorFileDoesNotExist
            ("NSURLErrorDomain", -1100) => return ErrorKind::NotFound,
            // NSFileNoSuchFileError, NSFileReadNoSuchFileError
            ("NSCocoaErrorDomain", 4 | 260) => return ErrorKind::NotFound,
            // ENOENT
            ("NSPOSIXErrorDomain", 2) => return ErrorKind::NotFound,
            _ => {}
        }
    }
    // SAFETY: the domain is a constant NSString.
    let av_domain = unsafe { AVFoundationErrorDomain }.map(|domain| domain.to_string());
    for link in chain {
        if Some(&link.domain) == av_domain.as_ref() {
            let code = AVError(link.code);
            if code == AVError::FileFormatNotRecognized
                || code == AVError::DecoderNotFound
                || code == AVError::ContentIsUnavailable
            {
                return ErrorKind::Unsupported;
            }
            if code == AVError::DecodeFailed
                || code == AVError::FileFailedToParse
                || code == AVError::FailedToParse
                || code == AVError::FailedToLoadMediaData
                || code == AVError::InvalidSampleCursor
            {
                return ErrorKind::Decode;
            }
            if code == AVError::ServerIncorrectlyConfigured {
                return ErrorKind::Network;
            }
        }
    }
    if chain.iter().any(|link| link.domain == "NSURLErrorDomain") {
        return ErrorKind::Network;
    }
    ErrorKind::Other
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(domain: &str, code: isize, text: &str) -> Link {
        Link {
            domain: domain.into(),
            code,
            text: text.into(),
        }
    }

    #[test]
    fn reads_the_http_status() {
        assert_eq!(http_status("HTTP 403: Forbidden"), Some(403));
        assert_eq!(http_status("The operation could not be completed"), None);
    }

    #[test]
    fn an_underlying_401_is_unauthorized() {
        let chain = [
            link(
                "AVFoundationErrorDomain",
                -11800,
                "The operation could not be completed",
            ),
            link("CoreMediaErrorDomain", -12937, "HTTP 401: Unauthorized"),
        ];
        assert_eq!(classify(&chain), ErrorKind::Unauthorized);
    }

    #[test]
    fn a_dropped_connection_is_network() {
        let chain = [link(
            "NSURLErrorDomain",
            -1004,
            "Could not connect to the server.",
        )];
        assert_eq!(classify(&chain), ErrorKind::Network);
    }
}
