//! Range requests through `NSURLSession`, for remuxing a Matroska or WebM
//! URL on the HLS server's thread. Each fetch blocks that thread (never
//! the main one) until the answer is in.

use std::io;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use gpui_av_remux::{Fetched, RangeSource};
use objc2_foundation::{
    NSData, NSError, NSHTTPURLResponse, NSMutableURLRequest, NSString, NSURL, NSURLResponse,
    NSURLSession,
};

/// How long one range request may take.
const TIMEOUT: Duration = Duration::from_secs(60);

/// A URL and the headers that go with every request for it.
pub(crate) struct UrlRanges {
    pub url: String,
    pub headers: Vec<(String, String)>,
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
        // SAFETY: the shared session's tasks call the handler on a
        // background queue; everything it captures is Send.
        let task = unsafe {
            NSURLSession::sharedSession().dataTaskWithRequest_completionHandler(&request, &handler)
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
