//! Matroska and WebM the browser will not open itself (Safari and Firefox
//! with Matroska, Safari with most WebM): remuxed to fragmented MP4 as it
//! plays (`gpui-av-remux`, nothing is decoded) and handed to the same
//! `<video>` element through Media Source Extensions, so the browser's own
//! decoders still play it.
//!
//! The file is fetched in parts with range requests: the header first,
//! then each segment as playback nears it, keeping about
//! [`AHEAD_SECONDS`] buffered ahead and dropping what is far behind.

use std::cell::Cell;
use std::rc::Rc;

use gpui_av::{Error, ErrorKind};
use gpui_av_remux::{Remuxer, SparseFile, SparseReader};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use super::{describe, sleep};

/// How much is fetched and appended ahead of the playhead.
const AHEAD_SECONDS: f64 = 30.0;
/// What is kept behind the playhead, for a short step back.
const BEHIND_SECONDS: f64 = 20.0;
/// The first fetch, which also tells whether the file is Matroska. Each
/// further fetch while reading the header doubles, up to [`MAX_CHUNK`].
const FIRST_CHUNK: u64 = 64 * 1024;
const MAX_CHUNK: u64 = 4 * 1024 * 1024;
/// The largest file taken whole from a server that ignores ranges.
const WHOLE_FILE_LIMIT: u64 = 256 * 1024 * 1024;
/// How often the feeder looks at the playhead when it has nothing to do.
const IDLE_MILLISECONDS: i32 = 200;

/// Where the file is, and the headers that go with each request.
#[derive(Clone)]
pub(crate) struct Remote {
    pub url: String,
    pub headers: Vec<(String, String)>,
}

/// What became of trying the file through Media Source Extensions.
pub(crate) enum Outcome {
    /// It is not Matroska or WebM: the element's own error stands.
    NotMatroska,
    /// It plays; dropping this stops the feeding.
    Playing(Feeding),
}

/// The feeding of one element, stopped when dropped.
pub(crate) struct Feeding {
    stop: Rc<Cell<bool>>,
    object_url: String,
}

impl Drop for Feeding {
    fn drop(&mut self) {
        self.stop.set(true);
        if let Err(error) = web_sys::Url::revoke_object_url(&self.object_url) {
            web_sys::console::warn_1(&error);
        }
    }
}

/// Fetches the start of the file and, if it is Matroska or WebM, starts
/// feeding `video` through a `MediaSource`. Later failures go to `fail`.
pub(crate) async fn start(
    video: web_sys::HtmlVideoElement,
    remote: Remote,
    fail: impl Fn(Error) + 'static,
) -> Result<Outcome, Error> {
    let file = SparseFile::new();
    fetch(&remote, &file, 0, Some(FIRST_CHUNK)).await?;
    let mut start = [0u8; 4];
    let is_matroska = {
        use std::io::Read;
        file.reader().read_exact(&mut start).is_ok() && gpui_av_remux::is_matroska(&start)
    };
    if !is_matroska {
        return Ok(Outcome::NotMatroska);
    }
    let remuxer = open(&remote, &file).await?;
    let mime = remuxer.mime_type();
    if !web_sys::MediaSource::is_type_supported(&mime) {
        return Err(Error::new(
            ErrorKind::Unsupported,
            format!(
                "this browser cannot play {} from this {} file, itself or through Media Source \
                 Extensions",
                remuxer.codecs(),
                remuxer.doc_type()
            ),
        ));
    }
    let source = web_sys::MediaSource::new().map_err(other)?;
    let object_url = web_sys::Url::create_object_url_with_source(&source).map_err(other)?;
    let opened = next_event(&source, "sourceopen");
    video.set_src(&object_url);
    opened.await.map_err(other)?;
    let buffer = source.add_source_buffer(&mime).map_err(other)?;
    source.set_duration(remuxer.duration().as_secs_f64());
    // The remuxer moves every time later so none is negative (B-frames,
    // audio priming); this moves them back.
    buffer.set_timestamp_offset(-remuxer.presentation_offset().as_secs_f64());
    let init = remuxer.init_segment();
    append(&buffer, &init).await?;

    let stop = Rc::new(Cell::new(false));
    let feeder = Feeder {
        video,
        source,
        buffer,
        remuxer,
        file,
        remote,
        stop: stop.clone(),
        last: None,
    };
    wasm_bindgen_futures::spawn_local(async move {
        let stopped = feeder.stop.clone();
        if let Err(error) = feeder.run().await
            && !stopped.get()
        {
            fail(error);
        }
    });
    Ok(Outcome::Playing(Feeding { stop, object_url }))
}

/// Reads the header, fetching what the remuxer asks for, more each time.
async fn open(remote: &Remote, file: &SparseFile) -> Result<Remuxer<SparseReader>, Error> {
    let mut chunk = FIRST_CHUNK;
    loop {
        match Remuxer::open(file.reader()) {
            Ok(remuxer) => return Ok(remuxer),
            Err(error) => {
                let Some(at) = error.missing() else {
                    return Err(remux_error(error));
                };
                chunk = (chunk * 2).min(MAX_CHUNK);
                let before = file.first_missing(at..at + 1);
                fetch(remote, file, at, Some(chunk)).await?;
                if file.first_missing(at..at + 1) == before {
                    return Err(Error::new(
                        ErrorKind::Decode,
                        "the file ends before its header does",
                    ));
                }
            }
        }
    }
}

fn remux_error(error: gpui_av_remux::RemuxError) -> Error {
    let kind = match error {
        gpui_av_remux::RemuxError::Invalid(_) => ErrorKind::Decode,
        gpui_av_remux::RemuxError::Io(_) => ErrorKind::Other,
        _ => ErrorKind::Unsupported,
    };
    Error::new(kind, error.to_string())
}

struct Feeder {
    video: web_sys::HtmlVideoElement,
    source: web_sys::MediaSource,
    buffer: web_sys::SourceBuffer,
    remuxer: Remuxer<SparseReader>,
    file: SparseFile,
    remote: Remote,
    stop: Rc<Cell<bool>>,
    /// The segment appended last.
    last: Option<usize>,
}

impl Feeder {
    async fn run(mut self) -> Result<(), Error> {
        let starts: Vec<f64> = self
            .remuxer
            .segments()
            .iter()
            .map(|segment| segment.start.as_secs_f64())
            .collect();
        loop {
            if self.stop.get() {
                return Ok(());
            }
            let now = self.video.current_time();
            let buffered_to = buffered_end(&self.video, now);
            if buffered_to.is_some_and(|end| end - now >= AHEAD_SECONDS) {
                sleep(IDLE_MILLISECONDS).await;
                continue;
            }
            // The segment that carries on from what is buffered here, or
            // the one the playhead is in.
            let from = buffered_to.map_or(now, |end| end + 0.1);
            let mut next = starts.iter().rposition(|start| *start <= from).unwrap_or(0);
            // The segment just appended does not reach far enough: its
            // tracks end a little apart (Matroska muxers put the sound just
            // before a keyframe in the next cluster), so the buffered range
            // stops short of its end. The next segment carries on.
            if self.last == Some(next) {
                next += 1;
            }
            if next >= starts.len() {
                if self.source.ready_state() == web_sys::MediaSourceReadyState::Open
                    && !self.buffer.updating()
                {
                    self.source.end_of_stream().map_err(other)?;
                }
                sleep(IDLE_MILLISECONDS).await;
                continue;
            }
            self.forget_behind(now).await?;
            let segment = self.segment(next).await?;
            if self.stop.get() {
                return Ok(());
            }
            if let Err(error) = append(&self.buffer, &segment).await {
                // Full: drop everything but what is around the playhead and
                // try once more.
                self.remove(0.0, (self.video.current_time() - 1.0).max(0.0))
                    .await?;
                append(&self.buffer, &segment).await.map_err(|_| error)?;
            }
            self.last = Some(next);
        }
    }

    /// Fetches segment `index`'s bytes and remuxes them.
    async fn segment(&mut self, index: usize) -> Result<Vec<u8>, Error> {
        let (start, end) = self
            .remuxer
            .segment_bytes(index)
            .ok_or_else(|| Error::new(ErrorKind::Other, "no such segment"))?;
        let end = end.or(self.file.length());
        if self
            .file
            .first_missing(start..end.unwrap_or(u64::MAX))
            .is_some()
        {
            fetch(
                &self.remote,
                &self.file,
                start,
                end.map(|end| end.saturating_sub(start)),
            )
            .await?;
        }
        let segment = self.remuxer.media_segment(index).map_err(remux_error);
        // The header's bytes stay; a segment's are used once.
        if let Some(end) = end {
            self.file.forget(start..end);
        }
        segment
    }

    async fn forget_behind(&self, now: f64) -> Result<(), Error> {
        let buffered = self.buffer.buffered().map_err(other)?;
        if buffered.length() > 0 {
            let first = buffered.start(0).map_err(other)?;
            if now - first > BEHIND_SECONDS * 1.5 {
                self.remove(first, now - BEHIND_SECONDS).await?;
            }
        }
        Ok(())
    }

    async fn remove(&self, from: f64, to: f64) -> Result<(), Error> {
        if to <= from {
            return Ok(());
        }
        let done = next_event(&self.buffer, "updateend");
        self.buffer.remove(from, to).map_err(other)?;
        done.await.map(drop).map_err(other)
    }
}

/// The end of the buffered range the playhead is in.
fn buffered_end(video: &web_sys::HtmlVideoElement, now: f64) -> Option<f64> {
    let ranges = video.buffered();
    (0..ranges.length()).find_map(|index| {
        let start = ranges.start(index).ok()?;
        let end = ranges.end(index).ok()?;
        // A playhead a little before a range (just after a seek) is in it.
        (start - 0.25 <= now && now <= end).then_some(end)
    })
}

/// Appends `bytes` and waits for the buffer to take them. A decode error
/// in them ends the stream, and the element reports it.
async fn append(buffer: &web_sys::SourceBuffer, bytes: &[u8]) -> Result<(), Error> {
    let done = next_event(buffer, "updateend");
    // The page's memory is shared with GPUI's workers, and a source
    // buffer takes no view of shared memory: copy the bytes out first.
    let copy = js_sys::Uint8Array::new_with_length(bytes.len() as u32);
    copy.copy_from(bytes);
    buffer
        .append_buffer_with_js_u8_array(&copy)
        .map_err(|error| {
            let full = error
                .dyn_ref::<web_sys::DomException>()
                .is_some_and(|error| error.name() == "QuotaExceededError");
            Error::new(
                if full {
                    ErrorKind::Other
                } else {
                    ErrorKind::Decode
                },
                describe(&error),
            )
        })?;
    done.await.map(drop).map_err(other)
}

/// Fetches `length` bytes (or to the end) from `start` into `file`, and
/// notes the file's length where the server says it.
async fn fetch(
    remote: &Remote,
    file: &SparseFile,
    start: u64,
    length: Option<u64>,
) -> Result<(), Error> {
    let window = web_sys::window().ok_or_else(|| Error::new(ErrorKind::Other, "no window"))?;
    let headers = web_sys::Headers::new().map_err(other)?;
    for (name, value) in &remote.headers {
        headers.set(name, value).map_err(other)?;
    }
    let range = match length {
        Some(length) => format!("bytes={start}-{}", start + length.max(1) - 1),
        None => format!("bytes={start}-"),
    };
    headers.set("Range", &range).map_err(other)?;
    let init = web_sys::RequestInit::new();
    init.set_headers(&headers);
    init.set_mode(web_sys::RequestMode::Cors);
    let response: web_sys::Response =
        JsFuture::from(window.fetch_with_str_and_init(&remote.url, &init))
            .await
            .map_err(|error| Error::new(ErrorKind::Network, describe(&error)))?
            .dyn_into()
            .map_err(other)?;
    let status = response.status();
    let whole = match status {
        206 => false,
        200 => true,
        404 | 410 => {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("the server answered {status}"),
            ));
        }
        401 | 403 => {
            return Err(Error::new(
                ErrorKind::Unauthorized,
                format!("the server answered {status}"),
            ));
        }
        _ => {
            return Err(Error::new(
                ErrorKind::Network,
                format!("the server answered {status}"),
            ));
        }
    };
    if whole {
        let size = response
            .headers()
            .get("Content-Length")
            .ok()
            .flatten()
            .and_then(|size| size.parse::<u64>().ok());
        if size.is_none_or(|size| size > WHOLE_FILE_LIMIT) {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "the server does not take range requests, which playing a large Matroska \
                 or WebM file in this browser needs",
            ));
        }
    }
    let body = JsFuture::from(response.array_buffer().map_err(other)?)
        .await
        .map_err(|error| Error::new(ErrorKind::Network, describe(&error)))?;
    let bytes = js_sys::Uint8Array::new(&body).to_vec();
    if whole {
        // The server ignored the range and sent the whole file.
        file.set_length(bytes.len() as u64);
        file.insert(0, bytes);
        return Ok(());
    }
    let total = response
        .headers()
        .get("Content-Range")
        .ok()
        .flatten()
        .and_then(|range| {
            range
                .rsplit_once('/')
                .and_then(|(_, total)| total.parse().ok())
        });
    if let Some(total) = total {
        file.set_length(total);
    } else if length.is_none_or(|length| (bytes.len() as u64) < length) {
        // Less than asked for: this is the end.
        file.set_length(start + bytes.len() as u64);
    }
    file.insert(start, bytes);
    Ok(())
}

/// Resolves when `target` next fires `name`. The listener is added now,
/// so an event fired before the await is not missed.
fn next_event(target: &web_sys::EventTarget, name: &str) -> JsFuture {
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        let options = web_sys::AddEventListenerOptions::new();
        options.set_once(true);
        if let Err(error) = target.add_event_listener_with_callback_and_add_event_listener_options(
            name, &resolve, &options,
        ) && let Err(error) = reject.call1(&JsValue::NULL, &error)
        {
            web_sys::console::warn_1(&error);
        }
    });
    JsFuture::from(promise)
}

fn other(error: JsValue) -> Error {
    Error::new(ErrorKind::Other, describe(&error))
}
