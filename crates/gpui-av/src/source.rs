use std::fmt;
use std::path::{Path, PathBuf};

/// Where the media is.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Location {
    /// A file on this machine.
    File(PathBuf),
    /// An `http` or `https` URL.
    Url(String),
}

/// What to play: a file or a URL, plus the request headers a URL needs.
///
/// A backend either sends every header with every request it makes for the
/// source (range requests included) or fails with
/// [`ErrorKind::Unsupported`](crate::ErrorKind::Unsupported). It never
/// drops them.
///
/// ```
/// use gpui_av::Source;
///
/// let film = Source::file("/Users/me/Movies/film.mkv");
/// let stream = Source::url("https://media.example.com/Videos/42/stream.mkv")
///     .header("Authorization", "MediaBrowser Token=\"abc\"");
/// assert_eq!(stream.headers().len(), 1);
/// assert!(film.path().is_some());
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct Source {
    location: Location,
    headers: Vec<(String, String)>,
}

impl Source {
    /// A file on this machine.
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self {
            location: Location::File(path.into()),
            headers: Vec::new(),
        }
    }

    /// An `http` or `https` URL.
    pub fn url(url: impl Into<String>) -> Self {
        Self {
            location: Location::Url(url.into()),
            headers: Vec::new(),
        }
    }

    /// Adds a request header, for example `Authorization`. Setting a name
    /// again (in any letter case) replaces its value.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        let (name, value) = (name.into(), value.into());
        match self
            .headers
            .iter_mut()
            .find(|(existing, _)| existing.eq_ignore_ascii_case(&name))
        {
            Some(entry) => *entry = (name, value),
            None => self.headers.push((name, value)),
        }
        self
    }

    /// Where the media is.
    pub fn location(&self) -> &Location {
        &self.location
    }

    /// The file path, when this is a file.
    pub fn path(&self) -> Option<&Path> {
        match &self.location {
            Location::File(path) => Some(path),
            Location::Url(_) => None,
        }
    }

    /// The URL, when this is a URL.
    pub fn as_url(&self) -> Option<&str> {
        match &self.location {
            Location::Url(url) => Some(url),
            Location::File(_) => None,
        }
    }

    /// The request headers, in the order they were added.
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }
}

// Header values are usually credentials, so they stay out of logs.
impl fmt::Debug for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.headers.iter().map(|(name, _)| name.as_str()).collect();
        f.debug_struct("Source")
            .field("location", &self.location)
            .field("headers", &names)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_set_twice_keeps_the_last_value() {
        let source = Source::url("https://example.test/a.mp4")
            .header("Authorization", "one")
            .header("authorization", "two");
        assert_eq!(
            source.headers(),
            [("authorization".to_string(), "two".to_string())]
        );
    }

    #[test]
    fn debug_output_hides_header_values() {
        let source = Source::url("https://example.test/a.mp4").header("Authorization", "secret");
        let debug = format!("{source:?}");
        assert!(debug.contains("Authorization"));
        assert!(!debug.contains("secret"));
    }
}
