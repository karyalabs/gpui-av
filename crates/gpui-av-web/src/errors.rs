//! From a media element's error to a gpui-av error.

use gpui_av::ErrorKind;

/// `MediaError.code` values.
pub(crate) const MEDIA_ERR_ABORTED: u16 = 1;
pub(crate) const MEDIA_ERR_NETWORK: u16 = 2;
pub(crate) const MEDIA_ERR_DECODE: u16 = 3;
pub(crate) const MEDIA_ERR_SRC_NOT_SUPPORTED: u16 = 4;

/// What a plain request for the media's URL said. A media element reports
/// "source not supported" for a missing file, a refused request and an
/// unknown format alike, so the backend asks again to tell them apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Probe {
    /// The server answered with this HTTP status.
    Status(u16),
    /// The server answered, but the page may not see the status (a
    /// cross-origin server without CORS headers).
    Hidden,
    /// No answer at all.
    Unreachable,
}

/// The kind of error for a media element's error code, given what a probe
/// of the URL said.
pub(crate) fn kind(code: u16, probe: Probe) -> ErrorKind {
    match probe {
        Probe::Status(401 | 403) => ErrorKind::Unauthorized,
        Probe::Status(404 | 410) => ErrorKind::NotFound,
        Probe::Status(status) if status >= 400 => ErrorKind::Network,
        Probe::Unreachable => ErrorKind::Network,
        // The bytes arrive, so the fault is in them.
        Probe::Status(_) | Probe::Hidden => match code {
            MEDIA_ERR_NETWORK => ErrorKind::Network,
            MEDIA_ERR_DECODE => ErrorKind::Decode,
            MEDIA_ERR_SRC_NOT_SUPPORTED => ErrorKind::Unsupported,
            _ => ErrorKind::Other,
        },
    }
}

/// A sentence for the error, with the browser's own detail when it gave
/// one.
pub(crate) fn message(code: u16, probe: Probe, detail: &str) -> String {
    let what = match (kind(code, probe), probe) {
        (ErrorKind::Unauthorized, Probe::Status(status)) => {
            format!("the server refused the request (HTTP {status})")
        }
        (ErrorKind::NotFound, Probe::Status(status)) => {
            format!("the server has no such media (HTTP {status})")
        }
        (ErrorKind::Network, Probe::Status(status)) => {
            format!("the server answered with HTTP {status}")
        }
        (ErrorKind::Network, Probe::Unreachable) => "the server could not be reached".to_string(),
        (ErrorKind::Network, _) => "the network failed while the media was loading".to_string(),
        (ErrorKind::Decode, _) => "the browser could not decode the media".to_string(),
        (ErrorKind::Unsupported, _) => {
            "the browser cannot play this format, or the data is not media".to_string()
        }
        _ if code == MEDIA_ERR_ABORTED => "loading the media was aborted".to_string(),
        _ => "the browser could not play the media".to_string(),
    };
    // When the HTTP status explains the failure, the browser's detail
    // ("format error") only misleads.
    let explained = matches!(probe, Probe::Status(status) if status >= 400);
    if detail.is_empty() || explained {
        what
    } else {
        format!("{what}: {detail}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_http_status_decides_when_the_request_itself_failed() {
        for code in [MEDIA_ERR_NETWORK, MEDIA_ERR_SRC_NOT_SUPPORTED] {
            assert_eq!(kind(code, Probe::Status(401)), ErrorKind::Unauthorized);
            assert_eq!(kind(code, Probe::Status(403)), ErrorKind::Unauthorized);
            assert_eq!(kind(code, Probe::Status(404)), ErrorKind::NotFound);
            assert_eq!(kind(code, Probe::Status(503)), ErrorKind::Network);
            assert_eq!(kind(code, Probe::Unreachable), ErrorKind::Network);
        }
    }

    #[test]
    fn the_media_error_decides_when_the_bytes_arrive() {
        for probe in [Probe::Status(200), Probe::Status(206), Probe::Hidden] {
            assert_eq!(kind(MEDIA_ERR_NETWORK, probe), ErrorKind::Network);
            assert_eq!(kind(MEDIA_ERR_DECODE, probe), ErrorKind::Decode);
            assert_eq!(
                kind(MEDIA_ERR_SRC_NOT_SUPPORTED, probe),
                ErrorKind::Unsupported
            );
            assert_eq!(kind(MEDIA_ERR_ABORTED, probe), ErrorKind::Other);
        }
    }

    #[test]
    fn the_message_names_the_status_and_keeps_the_browsers_detail() {
        assert_eq!(
            message(
                MEDIA_ERR_SRC_NOT_SUPPORTED,
                Probe::Status(401),
                "Format error"
            ),
            "the server refused the request (HTTP 401)"
        );
        assert_eq!(
            message(
                MEDIA_ERR_DECODE,
                Probe::Status(206),
                "PIPELINE_ERROR_DECODE"
            ),
            "the browser could not decode the media: PIPELINE_ERROR_DECODE"
        );
    }
}
