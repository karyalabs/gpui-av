//! URLs with request headers. The media engine's own HTTP loading cannot
//! send headers, so for those sources the engine reads a byte stream of
//! ours: range requests through WinHTTP, each with the source's headers on
//! the source's origin and none anywhere a redirect leads (as the other
//! backends do). The reads block, and the engine makes them on its own
//! work queue threads, never the main one.

use std::ffi::c_void;
use std::io::{self, Read, Seek, SeekFrom};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

use gpui_av::{Error, ErrorKind};
use gpui_av_remux::{Fetch, Fetched, HttpResponse, RangeReader, RangeSource};
use windows::Win32::Foundation::{E_FAIL, E_NOTIMPL, S_FALSE, S_OK, STG_E_ACCESSDENIED};
use windows::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ADDREQ_FLAG_ADD, WINHTTP_ADDREQ_FLAG_REPLACE,
    WINHTTP_DISABLE_REDIRECTS, WINHTTP_FLAG_SECURE, WINHTTP_OPEN_REQUEST_FLAGS,
    WINHTTP_OPTION_DISABLE_FEATURE, WINHTTP_QUERY_CONTENT_RANGE, WINHTTP_QUERY_CONTENT_TYPE,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_LOCATION, WINHTTP_QUERY_STATUS_CODE,
    WinHttpAddRequestHeaders, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetOption,
};
use windows::Win32::System::Com::{
    ISequentialStream_Impl, IStream, IStream_Impl, LOCKTYPE, STATFLAG, STATSTG, STGC, STGM_READ,
    STGTY_STREAM, STREAM_SEEK, STREAM_SEEK_CUR, STREAM_SEEK_END, STREAM_SEEK_SET,
};
use windows::core::{HRESULT, PCWSTR, Ref, implement, w};

use crate::url::{Url, header_lines};

/// Redirects followed for one request before giving up.
const MAX_REDIRECTS: usize = 10;

/// The last failure of a read, kept so the engine's error can say what the
/// server said rather than only "network".
pub(crate) type LastError = Arc<Mutex<Option<Error>>>;

/// A WinHTTP handle, closed on drop. WinHTTP handles may be used from any
/// thread.
struct Handle(NonNull<c_void>);

// SAFETY: WinHTTP handles are not tied to the thread that opened them.
unsafe impl Send for Handle {}
// SAFETY: the only handle shared between threads is the session, which
// WinHTTP lets many threads open requests on at once; request handles are
// made and closed by one thread.
unsafe impl Sync for Handle {}

impl Handle {
    fn new(raw: *mut c_void) -> io::Result<Self> {
        NonNull::new(raw)
            .map(Self)
            .ok_or_else(io::Error::last_os_error)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle is open and closed only here.
        if let Err(error) = unsafe { WinHttpCloseHandle(self.0.as_ptr()) } {
            crate::engine::log(&format!("closing an HTTP handle failed: {error}"));
        }
    }
}

struct Response {
    status: u32,
    location: Option<String>,
    content_type: Option<String>,
    content_range: Option<String>,
    body: Vec<u8>,
}

/// Requests for one source, with its headers on its origin: range requests
/// of its URL for the byte stream, and any URL for the HLS proxy.
pub(crate) struct UrlRanges {
    session: Handle,
    url: Url,
    headers: Vec<(String, String)>,
}

impl UrlRanges {
    pub fn new(url: &str, headers: &[(String, String)]) -> Result<Self, Error> {
        let url = Url::parse(url).ok_or_else(|| {
            Error::new(
                ErrorKind::Unsupported,
                format!("not an http or https URL: {url}"),
            )
        })?;
        // SAFETY: a plain WinHTTP call with static strings.
        let session = Handle::new(unsafe {
            WinHttpOpen(
                w!("gpui-av"),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            )
        })
        .map_err(|error| Error::new(ErrorKind::Network, format!("WinHTTP: {error}")))?;
        Ok(Self {
            session,
            url,
            headers: headers.to_vec(),
        })
    }

    /// GETs `url`, following redirects by hand: the headers go only to
    /// the source's origin. Returns the answer and the URL that gave it.
    fn get(
        &self,
        mut url: Url,
        range: Option<(u64, Option<u64>)>,
        limit: Option<u64>,
    ) -> io::Result<(Response, Url)> {
        let mut redirects = 0;
        loop {
            let response = self.request(&url, range, url.same_origin(&self.url), limit)?;
            if !matches!(response.status, 301 | 302 | 303 | 307 | 308) {
                return Ok((response, url));
            }
            redirects += 1;
            let next = response
                .location
                .as_deref()
                .and_then(|location| url.resolve(location));
            match next {
                Some(next) if redirects <= MAX_REDIRECTS => url = next,
                Some(_) => return Err(io::Error::other("too many redirects")),
                None => return Err(io::Error::other("a redirect with no usable Location")),
            }
        }
    }

    /// One GET of `url` (bytes `start` to `end` with a range), without
    /// following a redirect. With `limit`, reads at most one byte more
    /// than that of the body.
    fn request(
        &self,
        url: &Url,
        range: Option<(u64, Option<u64>)>,
        with_headers: bool,
        limit: Option<u64>,
    ) -> io::Result<Response> {
        let host = wide(&url.host);
        let path = wide(&url.path);
        let flags = if url.secure {
            WINHTTP_FLAG_SECURE
        } else {
            WINHTTP_OPEN_REQUEST_FLAGS(0)
        };
        let mut lines = match range {
            Some((start, Some(end))) => format!("Range: bytes={start}-{end}\r\n"),
            Some((start, None)) => format!("Range: bytes={start}-\r\n"),
            None => String::new(),
        };
        if with_headers {
            lines += &header_lines(&self.headers);
        }
        let lines: Vec<u16> = lines.encode_utf16().collect();
        // SAFETY: WinHTTP calls on handles this function opens and closes,
        // with NUL-terminated strings and buffers that outlive each call.
        unsafe {
            let connect = Handle::new(WinHttpConnect(
                self.session.0.as_ptr(),
                PCWSTR(host.as_ptr()),
                url.port,
                0,
            ))?;
            let request = Handle::new(WinHttpOpenRequest(
                connect.0.as_ptr(),
                w!("GET"),
                PCWSTR(path.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                flags,
            ))?;
            WinHttpSetOption(
                Some(request.0.as_ptr()),
                WINHTTP_OPTION_DISABLE_FEATURE,
                Some(&WINHTTP_DISABLE_REDIRECTS.to_ne_bytes()),
            )?;
            WinHttpAddRequestHeaders(
                request.0.as_ptr(),
                &lines,
                WINHTTP_ADDREQ_FLAG_ADD | WINHTTP_ADDREQ_FLAG_REPLACE,
            )?;
            WinHttpSendRequest(request.0.as_ptr(), None, None, 0, 0, 0)?;
            WinHttpReceiveResponse(request.0.as_ptr(), std::ptr::null_mut())?;
            let mut status = 0u32;
            let mut size = size_of::<u32>() as u32;
            WinHttpQueryHeaders(
                request.0.as_ptr(),
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some((&raw mut status).cast()),
                &mut size,
                std::ptr::null_mut(),
            )?;
            let location = query_text(&request, WINHTTP_QUERY_LOCATION);
            let content_range = query_text(&request, WINHTTP_QUERY_CONTENT_RANGE);
            let content_type = query_text(&request, WINHTTP_QUERY_CONTENT_TYPE);
            let mut body = Vec::new();
            if (200..300).contains(&status) {
                let mut chunk = vec![0u8; 64 * 1024];
                // A server that ignores the range sends the whole file;
                // reading one byte more than asked for is enough to tell.
                while limit.is_none_or(|limit| (body.len() as u64) <= limit) {
                    let mut read = 0u32;
                    WinHttpReadData(
                        request.0.as_ptr(),
                        chunk.as_mut_ptr().cast(),
                        chunk.len() as u32,
                        &mut read,
                    )?;
                    if read == 0 {
                        break;
                    }
                    body.extend_from_slice(&chunk[..read as usize]);
                }
            }
            Ok(Response {
                status,
                location,
                content_type,
                content_range,
                body,
            })
        }
    }
}

impl RangeSource for UrlRanges {
    fn fetch(&mut self, start: u64, length: u64) -> io::Result<Fetched> {
        let end = start + length.max(1) - 1;
        let (response, _) = self.get(self.url.clone(), Some((start, Some(end))), Some(length))?;
        let Response {
            status,
            content_range,
            body,
            ..
        } = response;
        match status {
            206 => {}
            // The whole file, from a server that ignores ranges: only good
            // when it fits in what was asked for.
            200 if start == 0 && body.len() as u64 <= length => {}
            200 => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "the server does not take range requests, which playing with request headers needs",
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
            200 => Some(body.len() as u64),
            _ => content_range.and_then(|range| {
                range
                    .rsplit_once('/')
                    .and_then(|(_, total)| total.trim().parse().ok())
            }),
        };
        Ok(Fetched {
            bytes: body,
            length,
        })
    }
}

/// HLS through the loopback proxy: every playlist, key and segment with
/// the source's headers on its origin.
impl Fetch for UrlRanges {
    fn get(&self, url: &str, range: Option<(u64, Option<u64>)>) -> io::Result<HttpResponse> {
        let parsed = Url::parse(url).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("not an http URL: {url}"),
            )
        })?;
        let (response, answered) = UrlRanges::get(self, parsed, range, None)?;
        Ok(HttpResponse {
            status: response.status as u16,
            content_type: response.content_type,
            content_range: response.content_range,
            url: answered.to_text(),
            body: response.body,
        })
    }
}

/// A header's value as text, when the response has it.
///
/// # Safety
/// `request` must be a request handle with a response received.
unsafe fn query_text(request: &Handle, level: u32) -> Option<String> {
    let mut size = 0u32;
    // The first call fails, saying how many bytes the value takes.
    // SAFETY: as the function's contract; a null buffer asks for the size.
    let sized = unsafe {
        WinHttpQueryHeaders(
            request.0.as_ptr(),
            level,
            PCWSTR::null(),
            None,
            &mut size,
            std::ptr::null_mut(),
        )
    };
    if sized.is_ok() || size == 0 {
        return None;
    }
    let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
    // SAFETY: the buffer holds `size` bytes.
    unsafe {
        WinHttpQueryHeaders(
            request.0.as_ptr(),
            level,
            PCWSTR::null(),
            Some(buffer.as_mut_ptr().cast()),
            &mut size,
            std::ptr::null_mut(),
        )
    }
    .ok()?;
    buffer.truncate(size as usize / 2);
    Some(String::from_utf16_lossy(&buffer))
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The error a failed read stands for.
pub(crate) fn error_from(error: &io::Error) -> Error {
    let kind = match error.kind() {
        io::ErrorKind::NotFound => ErrorKind::NotFound,
        io::ErrorKind::PermissionDenied => ErrorKind::Unauthorized,
        io::ErrorKind::Unsupported => ErrorKind::Unsupported,
        _ => ErrorKind::Network,
    };
    Error::new(kind, error.to_string())
}

/// A reader that has made its first request, and the length the server
/// said. Made on a background thread, before the engine gets the stream.
pub(crate) struct Opened {
    reader: RangeReader<UrlRanges>,
    length: u64,
}

impl Opened {
    /// Fetches the start of the file (blocking), to learn its length and
    /// fail early on a wrong token or a missing file.
    pub fn open(ranges: UrlRanges) -> Result<Self, Error> {
        let mut reader = RangeReader::new(ranges);
        let length = reader
            .seek(SeekFrom::End(0))
            .and_then(|length| reader.seek(SeekFrom::Start(0)).map(|_| length))
            .map_err(|error| error_from(&error))?;
        Ok(Self { reader, length })
    }

    /// The stream for `MFCreateMFByteStreamOnStream`. Failed reads are
    /// kept in `last_error`.
    pub fn into_stream(self, last_error: LastError) -> IStream {
        UrlStream {
            reader: Mutex::new(self.reader),
            length: self.length,
            last_error,
        }
        .into()
    }
}

/// A read-only COM stream over the range reader.
#[implement(IStream)]
struct UrlStream {
    reader: Mutex<RangeReader<UrlRanges>>,
    length: u64,
    last_error: LastError,
}

impl UrlStream {
    fn keep(&self, error: &io::Error) {
        crate::engine::log(&format!("reading over HTTP failed: {error}"));
        if let Ok(mut last) = self.last_error.lock() {
            *last = Some(error_from(error));
        }
    }
}

impl ISequentialStream_Impl for UrlStream_Impl {
    fn Read(&self, pv: *mut c_void, cb: u32, pcbread: *mut u32) -> HRESULT {
        let Ok(mut reader) = self.reader.lock() else {
            return E_FAIL;
        };
        if pv.is_null() {
            return E_FAIL;
        }
        // SAFETY: the caller hands a writable buffer of `cb` bytes.
        let buffer = unsafe { std::slice::from_raw_parts_mut(pv.cast::<u8>(), cb as usize) };
        let mut filled = 0;
        while filled < buffer.len() {
            match reader.read(&mut buffer[filled..]) {
                Ok(0) => break,
                Ok(count) => filled += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => {
                    self.keep(&error);
                    return E_FAIL;
                }
            }
        }
        if !pcbread.is_null() {
            // SAFETY: the caller's out pointer, checked for null.
            unsafe { *pcbread = filled as u32 };
        }
        if filled < buffer.len() { S_FALSE } else { S_OK }
    }

    fn Write(&self, _pv: *const c_void, _cb: u32, _pcbwritten: *mut u32) -> HRESULT {
        STG_E_ACCESSDENIED
    }
}

impl IStream_Impl for UrlStream_Impl {
    fn Seek(
        &self,
        dlibmove: i64,
        dworigin: STREAM_SEEK,
        plibnewposition: *mut u64,
    ) -> windows::core::Result<()> {
        let to = match dworigin {
            STREAM_SEEK_SET => SeekFrom::Start(dlibmove.max(0) as u64),
            STREAM_SEEK_CUR => SeekFrom::Current(dlibmove),
            STREAM_SEEK_END => SeekFrom::End(dlibmove),
            _ => return Err(E_FAIL.into()),
        };
        let mut reader = self.reader.lock().map_err(|_| E_FAIL)?;
        let position = reader.seek(to).map_err(|error| {
            self.keep(&error);
            windows::core::Error::from(E_FAIL)
        })?;
        if !plibnewposition.is_null() {
            // SAFETY: the caller's out pointer, checked for null.
            unsafe { *plibnewposition = position };
        }
        Ok(())
    }

    fn SetSize(&self, _libnewsize: u64) -> windows::core::Result<()> {
        Err(STG_E_ACCESSDENIED.into())
    }

    fn CopyTo(
        &self,
        _pstm: Ref<IStream>,
        _cb: u64,
        _pcbread: *mut u64,
        _pcbwritten: *mut u64,
    ) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Commit(&self, _grfcommitflags: &STGC) -> windows::core::Result<()> {
        Ok(())
    }

    fn Revert(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn LockRegion(
        &self,
        _liboffset: u64,
        _cb: u64,
        _dwlocktype: &LOCKTYPE,
    ) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn UnlockRegion(
        &self,
        _liboffset: u64,
        _cb: u64,
        _dwlocktype: u32,
    ) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Stat(&self, pstatstg: *mut STATSTG, _grfstatflag: &STATFLAG) -> windows::core::Result<()> {
        if pstatstg.is_null() {
            return Err(E_FAIL.into());
        }
        // SAFETY: the caller's out pointer, checked for null; a zeroed
        // STATSTG is a valid one with no name.
        unsafe {
            *pstatstg = STATSTG {
                r#type: STGTY_STREAM.0 as u32,
                cbSize: self.length,
                grfMode: STGM_READ,
                ..std::mem::zeroed()
            };
        }
        Ok(())
    }

    fn Clone(&self) -> windows::core::Result<IStream> {
        Err(E_NOTIMPL.into())
    }
}
