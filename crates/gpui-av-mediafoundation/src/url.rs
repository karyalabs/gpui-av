//! Just enough of HTTP URLs for range requests with headers: the parts
//! WinHTTP takes, the origin the headers belong to, and where a redirect
//! goes.

/// An `http` or `https` URL, split the way `WinHttpConnect` and
/// `WinHttpOpenRequest` take it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Url {
    pub secure: bool,
    /// Lower case, without brackets for an IPv6 address.
    pub host: String,
    pub port: u16,
    /// The path and query, from the first `/`; never empty.
    pub path: String,
}

impl Url {
    pub fn parse(text: &str) -> Option<Self> {
        let (scheme, rest) = text.split_once("://")?;
        let secure = match scheme.to_ascii_lowercase().as_str() {
            "http" => false,
            "https" => true,
            _ => return None,
        };
        let rest = rest.split('#').next().unwrap_or_default();
        let split = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, path) = rest.split_at(split);
        // User information is never sent.
        let authority = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        let (host, port) = match authority.strip_prefix('[') {
            Some(bracketed) => {
                let (host, after) = bracketed.split_once(']')?;
                (host, after.strip_prefix(':'))
            }
            None => match authority.rsplit_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (authority, None),
            },
        };
        if host.is_empty() {
            return None;
        }
        let port = match port {
            Some(port) if !port.is_empty() => port.parse().ok()?,
            _ if secure => 443,
            _ => 80,
        };
        let path = match path {
            "" => "/".to_string(),
            path if path.starts_with('?') => format!("/{path}"),
            path => path.to_string(),
        };
        Some(Self {
            secure,
            host: host.to_ascii_lowercase(),
            port,
            path,
        })
    }

    /// Where a redirect's `Location` points, from this URL.
    pub fn resolve(&self, location: &str) -> Option<Self> {
        let location = location.trim();
        if let Some(url) = Self::parse(location) {
            return Some(url);
        }
        if location.contains("://") {
            // Another scheme: not one to follow.
            return None;
        }
        if location.starts_with("//") {
            let scheme = if self.secure { "https:" } else { "http:" };
            return Self::parse(&format!("{scheme}{location}"));
        }
        let path_only = self.path.split('?').next().unwrap_or("/");
        let path = if location.starts_with('/') {
            location.to_string()
        } else if location.starts_with('?') {
            format!("{path_only}{location}")
        } else {
            let directory = &path_only[..path_only.rfind('/').map_or(0, |at| at + 1)];
            format!("{directory}{location}")
        };
        Some(Self {
            path: if path.is_empty() { "/".into() } else { path },
            ..self.clone()
        })
    }

    /// The URL as text, the default port left out.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn to_text(&self) -> String {
        let scheme = if self.secure { "https" } else { "http" };
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        let default = if self.secure { 443 } else { 80 };
        if self.port == default {
            format!("{scheme}://{host}{}", self.path)
        } else {
            format!("{scheme}://{host}:{}{}", self.port, self.path)
        }
    }

    /// Whether the two have the same scheme, host and port: the source's
    /// headers go only to its own origin.
    pub fn same_origin(&self, other: &Self) -> bool {
        self.secure == other.secure && self.host == other.host && self.port == other.port
    }
}

/// The headers as request lines (`Name: value\r\n`), leaving out any whose
/// name or value would break a line, and any `Range` (the request's own).
pub(crate) fn header_lines(headers: &[(String, String)]) -> String {
    let mut lines = String::new();
    for (name, value) in headers {
        let breaks = |text: &str| text.contains(['\r', '\n']);
        if name.is_empty()
            || name.contains(':')
            || breaks(name)
            || breaks(value)
            || name.eq_ignore_ascii_case("range")
        {
            continue;
        }
        lines += &format!("{name}: {value}\r\n");
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_urls() {
        let url =
            Url::parse("HTTPS://User:pw@Media.Example:8920/Videos/1/stream.mkv?static=true#t=3")
                .unwrap();
        assert_eq!(
            url,
            Url {
                secure: true,
                host: "media.example".into(),
                port: 8920,
                path: "/Videos/1/stream.mkv?static=true".into(),
            }
        );
        assert_eq!(Url::parse("http://host").unwrap().path, "/");
        assert_eq!(Url::parse("http://host?a=1").unwrap().path, "/?a=1");
        assert_eq!(Url::parse("http://host/").unwrap().port, 80);
        assert_eq!(Url::parse("https://host/").unwrap().port, 443);
        let v6 = Url::parse("http://[::1]:8096/a").unwrap();
        assert_eq!((v6.host.as_str(), v6.port), ("::1", 8096));
        assert!(Url::parse("ftp://host/a").is_none());
        assert!(Url::parse("http:///a").is_none());
        assert!(Url::parse("http://host:port/a").is_none());
        for text in [
            "https://media.example:8920/a?b",
            "http://[::1]/x",
            "http://h/",
        ] {
            assert_eq!(Url::parse(text).unwrap().to_text(), text);
        }
    }

    #[test]
    fn resolves_redirects() {
        let base = Url::parse("http://a.test:8096/videos/1/stream?x=1").unwrap();
        let other = base.resolve("https://cdn.test/signed?sig=2").unwrap();
        assert_eq!((other.host.as_str(), other.secure), ("cdn.test", true));
        assert_eq!(base.resolve("//b.test/c").unwrap().host, "b.test");
        assert!(!base.resolve("//b.test/c").unwrap().secure);
        assert_eq!(base.resolve("/root").unwrap().path, "/root");
        assert_eq!(
            base.resolve("other.mkv").unwrap().path,
            "/videos/1/other.mkv"
        );
        assert_eq!(base.resolve("?y=2").unwrap().path, "/videos/1/stream?y=2");
        assert!(base.resolve("data://x").is_none());
        assert!(base.resolve("/root").unwrap().same_origin(&base));
    }

    #[test]
    fn origins_are_scheme_host_and_port() {
        let base = Url::parse("http://a.test/x").unwrap();
        assert!(base.same_origin(&Url::parse("http://A.test:80/y").unwrap()));
        assert!(!base.same_origin(&Url::parse("https://a.test/x").unwrap()));
        assert!(!base.same_origin(&Url::parse("http://a.test:8080/x").unwrap()));
        assert!(!base.same_origin(&Url::parse("http://b.test/x").unwrap()));
    }

    #[test]
    fn header_lines_leave_out_what_would_break_a_request() {
        let headers = [
            ("Authorization", "MediaBrowser Token=\"abc\""),
            ("X-Bad", "a\r\nHost: evil"),
            ("Range", "bytes=0-"),
            ("", "x"),
            ("Bad:Name", "x"),
        ]
        .map(|(name, value)| (name.to_string(), value.to_string()));
        assert_eq!(
            header_lines(&headers),
            "Authorization: MediaBrowser Token=\"abc\"\r\n"
        );
    }
}
