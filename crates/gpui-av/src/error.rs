use std::fmt;

/// What went wrong, in terms an app can act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// No backend was given and no default backend is set.
    NoBackend,
    /// The file or URL does not exist.
    NotFound,
    /// The server refused the request (HTTP 401 or 403).
    Unauthorized,
    /// The source could not be fetched: no connection, a timeout, or another
    /// HTTP error.
    Network,
    /// The backend cannot play this source: an unknown container or codec, or
    /// a kind of source it does not take (for example request headers where
    /// the platform cannot send them).
    Unsupported,
    /// The media started to load and then failed: truncated or corrupt data.
    Decode,
    /// Anything else the backend reports.
    Other,
}

/// A playback error: a [kind](ErrorKind) and a message for a person.
///
/// Match on the kind to decide what to show; show the message as it is.
///
/// ```
/// use gpui_av::{Error, ErrorKind};
///
/// fn advice(error: &Error) -> &'static str {
///     match error.kind() {
///         ErrorKind::Unauthorized => "Sign in again.",
///         ErrorKind::NotFound => "This video is gone.",
///         ErrorKind::Network => "Check the connection.",
///         ErrorKind::Unsupported => "This video's format cannot play here.",
///         _ => "This video cannot play.",
///     }
/// }
/// let error = Error::new(ErrorKind::NotFound, "the server answered 404");
/// assert_eq!(advice(&error), "This video is gone.");
/// assert_eq!(error.message(), "the server answered 404");
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    message: String,
}

impl Error {
    /// Creates an error.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// The kind of failure.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The message, as the backend gave it.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}
