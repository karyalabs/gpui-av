//! GStreamer's errors, in gpui-av's terms.

use gpui_av::{Error, ErrorKind};

/// Maps a GStreamer error (its GError domain, code and message, plus the
/// debug text, which carries the HTTP status) to an error kind.
pub(crate) fn map(domain: &str, code: i32, message: &str, debug: Option<&str>) -> Error {
    let kind = match domain {
        // GstResourceError
        "gst-resource-error-quark" => match code {
            3 => ErrorKind::NotFound,
            15 => ErrorKind::Unauthorized,
            // OPEN_READ, OPEN_READ_WRITE, READ, SEEK: a source that could
            // not be read. souphttpsrc reports most HTTP failures as
            // OPEN_READ, with the status in the debug text.
            5 | 7 | 9 | 11 => http_status(debug).unwrap_or(ErrorKind::Network),
            _ => ErrorKind::Other,
        },
        // GstStreamError
        "gst-stream-error-quark" => match code {
            // NOT_IMPLEMENTED, TYPE_NOT_FOUND, WRONG_TYPE, CODEC_NOT_FOUND,
            // FORMAT, DECRYPT, DECRYPT_NOKEY
            3 | 4 | 5 | 6 | 11 | 12 | 13 => ErrorKind::Unsupported,
            // FAILED, DECODE, DEMUX
            1 | 7 | 9 => ErrorKind::Decode,
            _ => ErrorKind::Other,
        },
        // GstCoreError: MISSING_PLUGIN, NOT_IMPLEMENTED
        "gst-core-error-quark" if code == 12 || code == 3 => ErrorKind::Unsupported,
        _ => ErrorKind::Other,
    };
    Error::new(kind, message)
}

/// The kind an HTTP status in souphttpsrc's debug text points to.
fn http_status(debug: Option<&str>) -> Option<ErrorKind> {
    let debug = debug?;
    // "... (404), URL: ..." or "Not Found (404)"
    let status = debug
        .split(['(', ')'])
        .filter_map(|part| part.trim().parse::<u16>().ok())
        .find(|status| (400..600).contains(status))?;
    Some(match status {
        401 | 403 => ErrorKind::Unauthorized,
        404 | 410 => ErrorKind::NotFound,
        _ => ErrorKind::Network,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        let resource = "gst-resource-error-quark";
        assert_eq!(map(resource, 3, "", None).kind(), ErrorKind::NotFound);
        assert_eq!(
            map(resource, 5, "", Some("Unauthorized (401), URL: x")).kind(),
            ErrorKind::Unauthorized
        );
        assert_eq!(
            map(resource, 5, "", Some("Not Found (404), URL: x")).kind(),
            ErrorKind::NotFound
        );
        assert_eq!(map(resource, 5, "", None).kind(), ErrorKind::Network);
        let stream = "gst-stream-error-quark";
        assert_eq!(map(stream, 4, "", None).kind(), ErrorKind::Unsupported);
        assert_eq!(map(stream, 7, "", None).kind(), ErrorKind::Decode);
    }
}
