use std::cell::RefCell;
use std::ops::Range;
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use gpui::{App, AppContext, Corners, RenderImage, SharedString, Window, size};
use gpui_av::{
    Backend, Error, ErrorKind, EventSink, Location, MediaInfo, Placement, PlaybackState, Session,
    Source, Stats, Track, TrackKind,
};
use image::{Frame as ImageFrame, RgbaImage};
use smallvec::SmallVec;
use windows::Win32::Foundation::RECT;

use crate::http::{LastError, Opened, UrlRanges};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, IWICBitmap, IWICImagingFactory,
    WICBitmapCacheOnLoad, WICBitmapLockRead, WICRect,
};
use windows::Win32::Media::MediaFoundation::{
    CLSID_MFMediaEngineClassFactory, IMFAttributes, IMFGetService, IMFMediaEngine,
    IMFMediaEngineClassFactory, IMFMediaEngineEx, IMFMediaEngineNotify, IMFMediaEngineNotify_Impl,
    IMFTimedText, IMFTimedTextCue, IMFTimedTextNotify, IMFTimedTextNotify_Impl, IMFTimedTextTrack,
    MF_MEDIA_ENGINE_CALLBACK, MF_MEDIA_ENGINE_ERR_ABORTED, MF_MEDIA_ENGINE_ERR_DECODE,
    MF_MEDIA_ENGINE_ERR_ENCRYPTED, MF_MEDIA_ENGINE_ERR_NETWORK,
    MF_MEDIA_ENGINE_ERR_SRC_NOT_SUPPORTED, MF_MEDIA_ENGINE_EVENT_CANPLAY,
    MF_MEDIA_ENGINE_EVENT_DURATIONCHANGE, MF_MEDIA_ENGINE_EVENT_ENDED, MF_MEDIA_ENGINE_EVENT_ERROR,
    MF_MEDIA_ENGINE_EVENT_FORMATCHANGE, MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA,
    MF_MEDIA_ENGINE_EVENT_PAUSE, MF_MEDIA_ENGINE_EVENT_PLAYING, MF_MEDIA_ENGINE_EVENT_SEEKED,
    MF_MEDIA_ENGINE_EVENT_TIMEUPDATE, MF_MEDIA_ENGINE_EVENT_WAITING, MF_MEDIA_ENGINE_TIMEDTEXT,
    MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT, MF_MT_MAJOR_TYPE, MF_SD_LANGUAGE, MF_SD_STREAM_NAME,
    MF_TIMED_TEXT_CUE_EVENT, MF_TIMED_TEXT_CUE_EVENT_ACTIVE, MF_TIMED_TEXT_CUE_EVENT_CLEAR,
    MF_TIMED_TEXT_ERROR_CODE, MF_TIMED_TEXT_TRACK_KIND_METADATA, MF_VERSION, MFCreateAttributes,
    MFCreateMFByteStreamOnStream, MFMediaType_Audio, MFSTARTUP_FULL, MFStartup,
};
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::System::Variant::{VT_CLSID, VT_LPWSTR};
use windows::core::{BOOL, BSTR, HRESULT, Interface, PWSTR, Ref, implement};

/// Plays media with Media Foundation's media engine and draws it as GPUI
/// images.
///
/// Sources: file paths, and `http` and `https` URLs. A URL with request
/// headers (a media server's token) is read through a byte stream of our
/// own, with range requests that send the headers to the URL's origin and
/// to no other a redirect leads to; the engine's own loading cannot send
/// headers. That needs a server that takes range requests.
#[derive(Clone, Copy, Debug, Default)]
pub struct MediaFoundationBackend;

impl MediaFoundationBackend {
    /// The backend. It holds no state; one is enough for an app.
    pub fn new() -> Self {
        Self
    }

    /// Whether Media Foundation starts on this machine (Windows N editions
    /// and some servers ship without it).
    pub fn is_available() -> bool {
        startup().is_ok()
    }
}

impl Backend for MediaFoundationBackend {
    fn name(&self) -> &'static str {
        "mediafoundation"
    }

    fn presenter(&self) -> &'static str {
        "image"
    }

    fn open(
        &self,
        source: Source,
        events: EventSink,
        cx: &mut App,
    ) -> Result<Box<dyn Session>, Error> {
        let mut ranges = None;
        let mut proxy = None;
        let mut file = None;
        let url = match source.location() {
            Location::File(path) => {
                let path = std::path::absolute(path).map_err(|error| {
                    Error::new(ErrorKind::NotFound, format!("{}: {error}", path.display()))
                })?;
                file = Some(path.clone());
                path.to_string_lossy().into_owned()
            }
            Location::Url(url) => {
                if !source.headers().is_empty() {
                    let headers: Vec<(String, String)> = source
                        .headers()
                        .iter()
                        .map(|(name, value)| (name.to_string(), value.to_string()))
                        .collect();
                    let requests = UrlRanges::new(url, &headers)?;
                    if names_playlist(url) {
                        // HLS: the engine loads the playlists and segments
                        // itself, from the proxy, which sends the headers.
                        let started = gpui_av_remux::HlsProxy::start(url, requests);
                        let started = started.map_err(|error| {
                            Error::new(
                                ErrorKind::Other,
                                format!("cannot start the local proxy for {url}: {error}"),
                            )
                        })?;
                        let local = started.url().to_string();
                        proxy = Some(started);
                        local
                    } else {
                        ranges = Some(requests);
                        url.clone()
                    }
                } else {
                    url.clone()
                }
            }
            _ => {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    "the Media Foundation backend takes file paths and URLs",
                ));
            }
        };
        startup().map_err(|error| Error::new(ErrorKind::Unsupported, error.clone()))?;
        MfSession::open(&url, ranges, proxy, file, events, cx)
            .map(|session| Box::new(session) as Box<dyn Session>)
            .map_err(|error| Error::new(ErrorKind::Other, format!("media engine: {error}")))
    }
}

/// Starts Media Foundation once per process.
fn startup() -> Result<(), &'static String> {
    static STARTED: OnceLock<Result<(), String>> = OnceLock::new();
    STARTED
        .get_or_init(|| {
            // SAFETY: MFStartup may be called from any thread; it is never
            // paired with MFShutdown, so Media Foundation stays up for the
            // life of the process.
            unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }
                .map_err(|error| format!("Media Foundation did not start: {error}"))
        })
        .as_ref()
        .map(|_| ())
}

/// What the engine reports, sent from its own threads to the main thread.
#[derive(Clone, Copy, Debug)]
struct EngineEvent {
    event: u32,
    param1: usize,
    param2: u32,
}

#[implement(IMFMediaEngineNotify)]
struct Notify {
    sender: async_channel::Sender<EngineEvent>,
}

impl IMFMediaEngineNotify_Impl for Notify_Impl {
    fn EventNotify(&self, event: u32, param1: usize, param2: u32) -> windows::core::Result<()> {
        // A closed channel means the session is gone; nothing to tell.
        self.sender
            .try_send(EngineEvent {
                event,
                param1,
                param2,
            })
            .ok();
        Ok(())
    }
}

/// The cues showing now, by cue id, with their track id and text: the
/// engine hands in-band cues only to its timed text notifications, with
/// the track's cue list left empty.
type ActiveCues = Arc<Mutex<Vec<(u32, u32, String)>>>;

#[implement(IMFTimedTextNotify)]
struct TextNotify {
    active: ActiveCues,
}

impl TextNotify {
    fn active(&self) -> Option<std::sync::MutexGuard<'_, Vec<(u32, u32, String)>>> {
        self.active.lock().ok()
    }
}

impl IMFTimedTextNotify_Impl for TextNotify_Impl {
    fn TrackAdded(&self, _track: u32) {}
    fn TrackRemoved(&self, _track: u32) {}
    fn TrackSelected(&self, _track: u32, _selected: BOOL) {}
    fn TrackReadyStateChanged(&self, _track: u32) {}
    fn Error(&self, code: MF_TIMED_TEXT_ERROR_CODE, extended: HRESULT, track: u32) {
        log(&format!(
            "timed text error {} ({:#x}) on track {track}",
            code.0, extended.0
        ));
    }

    fn Cue(&self, event: MF_TIMED_TEXT_CUE_EVENT, _now: f64, cue: Ref<IMFTimedTextCue>) {
        let Some(mut active) = self.active() else {
            return;
        };
        if event == MF_TIMED_TEXT_CUE_EVENT_CLEAR {
            active.clear();
            return;
        }
        let Some(cue) = cue.as_ref() else {
            return;
        };
        // SAFETY: queries on the cue the engine handed in, for the length
        // of the call; each string is copied out and freed.
        let (id, track) = unsafe { (cue.GetId(), cue.GetTrackId()) };
        active.retain(|(shown, _, _)| *shown != id);
        if event == MF_TIMED_TEXT_CUE_EVENT_ACTIVE
            && let Some(text) = unsafe { cue_text(cue) }
        {
            active.push((id, track, text));
        }
    }

    fn Reset(&self) {
        if let Some(mut active) = self.active() {
            active.clear();
        }
    }
}

/// A cue's lines, joined by `\n`.
///
/// # Safety
///
/// `cue` must be a live cue from the engine.
unsafe fn cue_text(cue: &IMFTimedTextCue) -> Option<String> {
    // SAFETY: per the contract above; each string is copied out and freed.
    let lines: Vec<String> = unsafe {
        (0..cue.GetLineCount())
            .filter_map(|line| cue.GetLine(line).ok())
            .filter_map(|line| owned_string(line.GetText()))
            .collect()
    };
    (!lines.is_empty()).then(|| lines.join("\n"))
}

struct MfSession {
    inner: Rc<RefCell<Inner>>,
    _events: gpui::Task<()>,
    /// Opening a URL with headers: the first request, then the source.
    _source: Option<gpui::Task<()>>,
    /// Working out the length of a fragmented MP4 file the engine cannot.
    _length: Option<gpui::Task<()>>,
}

struct Inner {
    engine: IMFMediaEngine,
    sink: EventSink,
    wic: IWICImagingFactory,
    /// The bitmap frames are copied into, remade when the size changes.
    bitmap: Option<(IWICBitmap, u32, u32)>,
    loaded: bool,
    failed: bool,
    pending_seek: Option<Duration>,
    /// The image on screen, and those to free on the next paint.
    shown: Option<Arc<RenderImage>>,
    retired: Vec<Arc<RenderImage>>,
    presented: u64,
    /// The frames drawn since the last seek, for telling a stalled
    /// picture from a slow one.
    run: FrameRun,
    /// When the window last asked for a frame: a player off screen draws
    /// none, and that is no stall.
    last_paint: Option<Instant>,
    /// The engine's stream selection, where the system has it.
    streams: Option<IMFMediaEngineEx>,
    /// The audio streams, by their index among all streams, with their
    /// labels, read when the metadata loads.
    audio: Vec<(u32, Track)>,
    /// An audio choice made before the metadata loaded.
    pending_audio: Option<usize>,
    /// The engine's timed text, for subtitles inside the media.
    timed: Option<IMFTimedText>,
    /// The in-band cues showing now, kept by the timed text notifications.
    active_cues: ActiveCues,
    /// The text tracks, by their timed text id, read when the metadata
    /// loads.
    text: Vec<(u32, Track)>,
    /// The text track on, by index into `text`.
    text_on: Option<usize>,
    /// The text track the app chose, kept for tracks the engine lists
    /// late: in-band text tracks can appear after the metadata loads.
    text_wanted: Option<usize>,
    /// A text choice made before the metadata loaded.
    pending_text: Option<Option<usize>>,
    /// A URL with headers whose first request is still out: the engine
    /// has no source yet.
    waiting_for_source: bool,
    /// Play or pause asked for while waiting for the source.
    wanted_playing: Option<bool>,
    /// The last failed read of our own byte stream, which says more than
    /// the engine's error.
    http_error: Option<LastError>,
    /// The length of a fragmented MP4 with an empty header, added up from
    /// its fragments: the engine reports none for one.
    fragments_length: Option<Duration>,
    /// The loopback proxy HLS with headers plays through.
    proxy: Option<gpui_av_remux::HlsProxy>,
}

/// The presentation times of the first and last frame of a run of frames,
/// in seconds, and how many there were.
#[derive(Clone, Copy, Debug, Default)]
struct FrameRun {
    first: f64,
    last: f64,
    count: u32,
}

impl FrameRun {
    fn push(&mut self, at: f64) {
        if self.count == 0 {
            self.first = at;
        }
        self.last = at;
        self.count += 1;
    }

    /// Whether the picture has stopped while the clock reached `position`.
    /// It counts only once a run is long enough to know the frame rate,
    /// and a gap is a stall only when it is many frames long and over
    /// [`STALL_SECONDS`], so a slideshow or a single cover image is never
    /// one.
    fn stalled_at(&self, position: f64) -> bool {
        if self.count < 10 {
            return false;
        }
        let interval = (self.last - self.first) / f64::from(self.count - 1);
        position - self.last > (interval * 10.0).max(STALL_SECONDS)
    }
}

impl MfSession {
    fn open(
        url: &str,
        ranges: Option<UrlRanges>,
        proxy: Option<gpui_av_remux::HlsProxy>,
        file: Option<std::path::PathBuf>,
        sink: EventSink,
        cx: &mut App,
    ) -> windows::core::Result<Self> {
        let (sender, receiver) = async_channel::unbounded();
        // SAFETY: plain COM construction calls; every interface returned is
        // owned by the value holding it and released on drop.
        let (engine, wic) = unsafe {
            let factory: IMFMediaEngineClassFactory =
                CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER)?;
            let mut attributes: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attributes, 2)?;
            let attributes = attributes.ok_or_else(windows::core::Error::empty)?;
            let notify: IMFMediaEngineNotify = Notify { sender }.into();
            attributes.SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify)?;
            // With no window or DirectComposition visual, the engine runs as
            // a frame server: we pull each frame in this format.
            attributes.SetUINT32(
                &MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT,
                DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32,
            )?;
            let engine: IMFMediaEngine = factory.CreateInstance(0, &attributes)?;
            let wic: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            (engine, wic)
        };
        // Subtitles inside the media come through the engine's timed text
        // (Windows 8.1 and later), once in-band text is on: before the
        // source is set.
        // SAFETY: a service lookup and a setting on the new engine.
        let timed = unsafe {
            engine
                .cast::<IMFGetService>()
                .and_then(|service| service.GetService::<IMFTimedText>(&MF_MEDIA_ENGINE_TIMEDTEXT))
                .and_then(|timed| timed.SetInBandEnabled(true).map(|()| timed))
        };
        let active_cues = ActiveCues::default();
        let timed = timed.and_then(|timed| {
            let notify: IMFTimedTextNotify = TextNotify {
                active: active_cues.clone(),
            }
            .into();
            // SAFETY: registering a callback object the engine keeps a
            // reference to.
            unsafe { timed.RegisterNotifications(&notify) }.map(|()| timed)
        });
        let timed = match timed {
            Ok(timed) => Some(timed),
            Err(error) => {
                log(&format!(
                    "no timed text, so no subtitles inside the media: {error}"
                ));
                None
            }
        };
        if ranges.is_none() {
            // SAFETY: a plain engine command on the main thread.
            unsafe { engine.SetSource(&BSTR::from(url))? };
        }
        // Windows 8 and later have the extended engine, with stream
        // selection.
        let streams = engine.cast::<IMFMediaEngineEx>().ok();
        let inner = Rc::new(RefCell::new(Inner {
            engine,
            sink,
            wic,
            bitmap: None,
            loaded: false,
            failed: false,
            pending_seek: None,
            shown: None,
            retired: Vec::new(),
            presented: 0,
            run: FrameRun::default(),
            last_paint: None,
            streams,
            audio: Vec::new(),
            pending_audio: None,
            timed,
            active_cues,
            text: Vec::new(),
            text_on: None,
            text_wanted: None,
            pending_text: None,
            waiting_for_source: ranges.is_some(),
            wanted_playing: None,
            http_error: None,
            fragments_length: None,
            proxy,
        }));
        inner.borrow().sink.state(PlaybackState::Loading);
        let weak: Weak<RefCell<Inner>> = Rc::downgrade(&inner);
        let events = cx.spawn(async move |_| {
            while let Ok(event) = receiver.recv().await {
                let Some(inner) = weak.upgrade() else { break };
                inner.borrow_mut().handle(event);
            }
        });
        let source = ranges.map(|ranges| {
            let opening = cx.background_spawn(async move { Opened::open(ranges) });
            let weak = Rc::downgrade(&inner);
            let url = url.to_string();
            cx.spawn(async move |_| {
                let opened = opening.await;
                if let Some(inner) = weak.upgrade() {
                    inner.borrow_mut().set_stream(opened, &url);
                }
            })
        });
        let length = file.map(|path| {
            let reading = cx.background_spawn(async move {
                let file = std::fs::File::open(&path)?;
                gpui_av_remux::fragmented_mp4_length(&mut std::io::BufReader::new(file))
            });
            let weak = Rc::downgrade(&inner);
            cx.spawn(async move |_| {
                let length = match reading.await {
                    Ok(length) => length,
                    Err(error) => {
                        log(&format!("reading the fragments' length failed: {error}"));
                        None
                    }
                };
                if let (Some(length), Some(inner)) = (length, weak.upgrade()) {
                    let mut inner = inner.borrow_mut();
                    inner.fragments_length = Some(length);
                    if inner.loaded && !inner.failed {
                        inner.report_info();
                    }
                }
            })
        });
        Ok(Self {
            inner,
            _events: events,
            _source: source,
            _length: length,
        })
    }
}

/// A time from the engine (seconds as a float) as a duration, when it is a
/// real one.
fn seconds(value: f64) -> Option<Duration> {
    Duration::try_from_secs_f64(value).ok()
}

impl Inner {
    /// Gives the engine our byte stream for a URL with headers, once its
    /// first request is answered.
    fn set_stream(&mut self, opened: Result<Opened, Error>, url: &str) {
        self.waiting_for_source = false;
        if self.failed {
            return;
        }
        let opened = match opened {
            Ok(opened) => opened,
            Err(error) => {
                self.failed = true;
                self.sink.fail(error);
                return;
            }
        };
        let Some(streams) = &self.streams else {
            self.failed = true;
            self.sink.fail(Error::new(
                ErrorKind::Unsupported,
                "request headers need the media engine of Windows 8 or later",
            ));
            return;
        };
        let last_error = LastError::default();
        self.http_error = Some(last_error.clone());
        let stream = opened.into_stream(last_error);
        // SAFETY: COM calls on the main thread with live objects; the
        // engine keeps its own reference to the byte stream.
        let set = unsafe {
            MFCreateMFByteStreamOnStream(&stream)
                .and_then(|bytes| streams.SetSourceFromByteStream(&bytes, &BSTR::from(url)))
        };
        if let Err(error) = set {
            self.failed = true;
            self.sink.fail(Error::new(
                ErrorKind::Other,
                format!("media engine: {error}"),
            ));
            return;
        }
        if self.wanted_playing.take() == Some(true) {
            // SAFETY: a plain engine command on the main thread.
            if let Err(error) = unsafe { self.engine.Play() } {
                log(&format!("play failed: {error}"));
            }
        }
    }

    fn handle(&mut self, event: EngineEvent) {
        if self.failed {
            return;
        }
        let kind = event.event as i32;
        if trace() {
            // SAFETY: a plain engine query on the main thread.
            let position = unsafe { self.engine.GetCurrentTime() };
            eprintln!(
                "mediafoundation: event {kind} ({}, {:#x}) at {position:.3} s",
                event.param1, event.param2
            );
        }
        if kind == MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA.0 {
            self.loaded = true;
            self.read_streams();
            if let Some(index) = self.pending_audio.take() {
                self.select_audio(index);
            }
            self.read_text_tracks();
            // Subtitles start off, as on every backend.
            let text = self.pending_text.take().flatten();
            self.select_text(text);
            self.report_info();
            if let Some(position) = self.pending_seek.take() {
                self.seek(position);
            }
        } else if kind == MF_MEDIA_ENGINE_EVENT_DURATIONCHANGE.0
            || kind == MF_MEDIA_ENGINE_EVENT_FORMATCHANGE.0
        {
            if self.loaded {
                self.report_info();
            }
        } else if kind == MF_MEDIA_ENGINE_EVENT_CANPLAY.0 {
            self.find_late_text_tracks();
            // SAFETY: plain engine queries on the main thread.
            if unsafe { self.engine.IsPaused() }.as_bool() {
                self.sink.state(PlaybackState::Ready);
            }
        } else if kind == MF_MEDIA_ENGINE_EVENT_PLAYING.0 {
            self.find_late_text_tracks();
            self.sink.state(PlaybackState::Playing);
        } else if kind == MF_MEDIA_ENGINE_EVENT_WAITING.0 {
            self.sink.state(PlaybackState::Buffering);
        } else if kind == MF_MEDIA_ENGINE_EVENT_PAUSE.0 {
            // SAFETY: as above.
            let ended = unsafe { self.engine.IsEnded() }.as_bool();
            if !ended {
                self.sink.state(PlaybackState::Paused);
            }
        } else if kind == MF_MEDIA_ENGINE_EVENT_TIMEUPDATE.0 {
            self.find_late_text_tracks();
            if trace() {
                self.trace_text();
            }
            // A file cut off inside its media data can also leave the
            // engine playing nothing with its clock running on past the
            // stated length, with no end and no error (seen on Windows
            // Server 2025).
            // SAFETY: plain engine queries on the main thread.
            let (position, length) =
                unsafe { (self.engine.GetCurrentTime(), self.engine.GetDuration()) };
            let painting = self
                .last_paint
                .is_some_and(|painted| painted.elapsed() < Duration::from_millis(500));
            if painting && self.run.stalled_at(position) {
                self.failed = true;
                self.sink.fail(Error::new(
                    ErrorKind::Decode,
                    format!(
                        "the picture stopped at {:.1} s while the clock ran on; the file is cut off or damaged",
                        self.run.last
                    ),
                ));
            } else if length.is_finite() && position - length > EARLY_END_SECONDS {
                self.failed = true;
                self.sink.fail(Error::new(
                    ErrorKind::Decode,
                    format!(
                        "the media ran out before its stated length of {length:.1} s; the file is cut off or damaged"
                    ),
                ));
            }
        } else if kind == MF_MEDIA_ENGINE_EVENT_ENDED.0 {
            // A file cut off inside its media data ends where the data
            // stops, with no error; its header still states the full
            // length. Ending well short of it is a broken file, not an end.
            // SAFETY: plain engine queries on the main thread.
            let (position, length) =
                unsafe { (self.engine.GetCurrentTime(), self.engine.GetDuration()) };
            if length.is_finite() && length - position > EARLY_END_SECONDS {
                self.failed = true;
                self.sink.fail(Error::new(
                    ErrorKind::Decode,
                    format!(
                        "the media stopped at {position:.1} s, before its stated length of {length:.1} s"
                    ),
                ));
            } else {
                self.sink.state(PlaybackState::Ended);
            }
        } else if kind == MF_MEDIA_ENGINE_EVENT_SEEKED.0 {
            self.run = FrameRun::default();
            // SAFETY: as above.
            let position = seconds(unsafe { self.engine.GetCurrentTime() }).unwrap_or_default();
            self.sink.seeked(position);
        } else if kind == MF_MEDIA_ENGINE_EVENT_ERROR.0 {
            self.failed = true;
            let read_error = self
                .http_error
                .as_ref()
                .and_then(|last| last.lock().ok().and_then(|mut last| last.take()))
                .or_else(|| {
                    let failure = self.proxy.as_ref()?.failure()?;
                    Some(proxy_error(failure))
                });
            self.sink.fail(
                read_error.unwrap_or_else(|| error(event.param1, HRESULT(event.param2 as i32))),
            );
        }
    }

    fn report_info(&self) {
        // SAFETY: plain engine queries on the main thread.
        let info = unsafe {
            let has_video = self.engine.HasVideo().as_bool();
            let (mut width, mut height) = (0u32, 0u32);
            let natural_size = (has_video
                && self
                    .engine
                    .GetNativeVideoSize(Some(&mut width), Some(&mut height))
                    .is_ok()
                && width > 0
                && height > 0)
                .then(|| size(width, height));
            MediaInfo {
                duration: seconds(self.engine.GetDuration())
                    .filter(|duration| !duration.is_zero())
                    .or(self.fragments_length),
                natural_size,
                has_video,
                has_audio: Some(self.engine.HasAudio().as_bool()),
                tracks: self
                    .audio
                    .iter()
                    .chain(self.text.iter())
                    .map(|(_, track)| track.clone())
                    .collect(),
                audio_track: self.audio_track(),
                text_track: self.text_on,
                ..MediaInfo::default()
            }
        };
        self.sink.info(info);
    }

    /// Lists the audio streams. Text streams are listed by
    /// [`Inner::read_text_tracks`], from the engine's timed text.
    fn read_streams(&mut self) {
        let Some(streams) = &self.streams else {
            return;
        };
        // SAFETY: stream queries on a loaded engine, on the main thread.
        let count = unsafe { streams.GetNumberOfStreams() }.unwrap_or(0);
        let mut audio = Vec::new();
        for stream in 0..count {
            if stream_guid(streams, stream, &MF_MT_MAJOR_TYPE) != Some(MFMediaType_Audio) {
                continue;
            }
            let mut track = Track::new(TrackKind::Audio, audio.len());
            if let Some(language) = stream_string(streams, stream, &MF_SD_LANGUAGE) {
                track = track.with_language(language);
            }
            if let Some(name) = stream_string(streams, stream, &MF_SD_STREAM_NAME) {
                track = track.with_title(name);
            }
            audio.push((stream, track));
        }
        self.audio = audio;
        if trace() {
            for stream in 0..count {
                eprintln!(
                    "mediafoundation: stream {stream} major type {:?}",
                    stream_guid(streams, stream, &MF_MT_MAJOR_TYPE)
                );
            }
            if let Some(timed) = &self.timed {
                // SAFETY: timed text queries on the main thread.
                let (in_band, listed) = unsafe {
                    (
                        timed.IsInBandEnabled().as_bool(),
                        timed.GetTextTracks().map(|list| list.GetLength()).ok(),
                    )
                };
                eprintln!("mediafoundation: timed text in band {in_band}, tracks {listed:?}");
            }
        }
    }

    /// Lists the text tracks inside the media.
    fn read_text_tracks(&mut self) {
        let Some(timed) = &self.timed else {
            return;
        };
        // SAFETY: timed text queries on a loaded engine, on the main thread.
        let tracks = unsafe {
            let Ok(list) = timed.GetTextTracks() else {
                return;
            };
            (0..list.GetLength())
                .filter_map(|index| list.GetTrack(index).ok())
                .filter(|track| track.GetTrackKind() != MF_TIMED_TEXT_TRACK_KIND_METADATA)
                .enumerate()
                .map(|(index, track)| {
                    let mut entry = Track::new(TrackKind::Text, index);
                    if let Some(language) = owned_string(track.GetLanguage()) {
                        entry = entry.with_language(language);
                    }
                    if let Some(label) = owned_string(track.GetLabel()) {
                        entry = entry.with_title(label);
                    }
                    (track.GetId(), entry)
                })
                .collect()
        };
        self.text = tracks;
    }

    /// Lists text tracks the engine added after the metadata loaded, and
    /// reports them.
    fn find_late_text_tracks(&mut self) {
        if !self.loaded || !self.text.is_empty() || self.timed.is_none() {
            return;
        }
        self.read_text_tracks();
        if !self.text.is_empty() {
            log(&format!(
                "{} text tracks listed after the metadata",
                self.text.len()
            ));
            self.select_text(self.text_wanted);
            self.report_info();
        }
    }

    /// Turns on the text track at `index`, or every one off for `None`.
    fn select_text(&mut self, index: Option<usize>) {
        self.text_wanted = index;
        let Some(timed) = &self.timed else {
            return;
        };
        for (position, (id, _)) in self.text.iter().enumerate() {
            // SAFETY: a timed text command on the main thread.
            if let Err(error) = unsafe { timed.SelectTrack(*id, Some(position) == index) } {
                log(&format!("could not switch text track {id}: {error}"));
            }
        }
        self.text_on = index.filter(|index| *index < self.text.len());
    }

    /// Prints the state of the text track that is on.
    fn trace_text(&self) {
        let (Some(timed), Some(on)) = (&self.timed, self.text_on) else {
            return;
        };
        let Some((id, _)) = self.text.get(on) else {
            return;
        };
        // SAFETY: timed text queries on the main thread.
        unsafe {
            let Ok(track) = timed
                .GetTextTracks()
                .and_then(|list| list.GetTrackById(*id))
            else {
                eprintln!("mediafoundation: text track {id} not found");
                return;
            };
            let cues = track.GetCueList().ok();
            let count = cues.as_ref().map(|cues| cues.GetLength());
            let first = cues
                .as_ref()
                .and_then(|cues| cues.GetCueByIndex(0).ok())
                .map(|cue| (cue.GetStartTime(), cue.GetDuration(), cue.GetLineCount()));
            eprintln!(
                "mediafoundation: text track {id} at {:.3} s: active {}, ready {:?}, error {:?}, cues {count:?}, first {first:?}, showing {:?}",
                self.engine.GetCurrentTime(),
                track.IsActive().as_bool(),
                track.GetReadyState(),
                track.GetErrorCode(),
                self.active_cues.lock().map(|active| active.clone()).ok(),
            );
        }
    }

    /// The text of the cues on screen now, on the text track that is on.
    fn current_text(&self) -> Option<String> {
        let timed = self.timed.as_ref()?;
        let (id, _) = self.text.get(self.text_on?)?;
        // SAFETY: timed text queries on the main thread; each string is
        // copied out and freed.
        unsafe {
            let now = self.engine.GetCurrentTime();
            let mut lines: Vec<String> = self
                .active_cues
                .lock()
                .map(|active| {
                    active
                        .iter()
                        .filter(|(_, track, _)| track == id)
                        .map(|(_, _, text)| text.clone())
                        .collect()
                })
                .unwrap_or_default();
            // Cues from a separate text file are in the track's cue list.
            let track: IMFTimedTextTrack = timed.GetTextTracks().ok()?.GetTrackById(*id).ok()?;
            let cues = track.GetCueList().ok()?;
            for index in 0..cues.GetLength() {
                let Ok(cue) = cues.GetCueByIndex(index) else {
                    continue;
                };
                let start = cue.GetStartTime();
                if !(start <= now && now < start + cue.GetDuration()) {
                    continue;
                }
                if let Some(text) = cue_text(&cue)
                    && !lines.contains(&text)
                {
                    lines.push(text);
                }
            }
            (!lines.is_empty()).then(|| lines.join("\n"))
        }
    }

    fn audio_track(&self) -> Option<usize> {
        let streams = self.streams.as_ref()?;
        self.audio.iter().position(|(stream, _)| {
            // SAFETY: a stream query on a loaded engine.
            unsafe { streams.GetStreamSelection(*stream) }.is_ok_and(|on| on.as_bool())
        })
    }

    fn select_audio(&mut self, index: usize) {
        let Some(streams) = &self.streams else {
            return;
        };
        if index >= self.audio.len() {
            return;
        }
        // SAFETY: stream selection on a loaded engine, on the main thread.
        let result = unsafe {
            self.audio
                .iter()
                .enumerate()
                .try_for_each(|(position, (stream, _))| {
                    streams.SetStreamSelection(*stream, position == index)
                })
                .and_then(|()| streams.ApplyStreamSelections())
        };
        if let Err(error) = result {
            log(&format!("could not switch the audio track: {error}"));
        }
    }

    fn seek(&mut self, position: Duration) {
        if !self.loaded {
            self.pending_seek = Some(position);
            return;
        }
        self.run = FrameRun::default();
        // SAFETY: a plain engine command on the main thread.
        if let Err(error) = unsafe { self.engine.SetCurrentTime(position.as_secs_f64()) } {
            log(&format!("seek failed: {error}"));
        }
    }

    /// Copies the frame due now into a new image, when the engine has one.
    fn take_frame(&mut self) {
        let mut pts = 0i64;
        // SAFETY: OnVideoStreamTick fills `pts`; its HRESULT is S_OK only
        // when a new frame is due (S_FALSE, also a success, means none),
        // which the wrapper method cannot tell apart, so the vtable is
        // called directly.
        let hr = unsafe {
            (Interface::vtable(&self.engine).OnVideoStreamTick)(
                Interface::as_raw(&self.engine),
                &mut pts,
            )
        };
        if hr != windows::Win32::Foundation::S_OK {
            return;
        }
        match self.copy_frame() {
            Ok(Some(image)) => {
                if let Some(old) = self.shown.replace(image) {
                    self.retired.push(old);
                }
                self.presented += 1;
                // OnVideoStreamTick gives the time in 100 ns units.
                self.run.push(pts as f64 / 10_000_000.0);
            }
            Ok(None) => {}
            Err(error) => log(&format!("could not copy a frame: {error}")),
        }
    }

    fn copy_frame(&mut self) -> windows::core::Result<Option<Arc<RenderImage>>> {
        let (mut width, mut height) = (0u32, 0u32);
        // SAFETY: plain engine queries and WIC calls on the main thread; the
        // locked data is read only while the lock lives, within the size
        // WIC reports.
        unsafe {
            self.engine
                .GetNativeVideoSize(Some(&mut width), Some(&mut height))?;
            if width == 0 || height == 0 {
                return Ok(None);
            }
            let bitmap = match &self.bitmap {
                Some((bitmap, w, h)) if (*w, *h) == (width, height) => bitmap.clone(),
                _ => {
                    let bitmap = self.wic.CreateBitmap(
                        width,
                        height,
                        &GUID_WICPixelFormat32bppBGRA,
                        WICBitmapCacheOnLoad,
                    )?;
                    self.bitmap = Some((bitmap.clone(), width, height));
                    bitmap
                }
            };
            let target = RECT {
                left: 0,
                top: 0,
                right: width as i32,
                bottom: height as i32,
            };
            self.engine
                .TransferVideoFrame(&bitmap, None, &target, None)?;
            let area = WICRect {
                X: 0,
                Y: 0,
                Width: width as i32,
                Height: height as i32,
            };
            let lock = bitmap.Lock(&area, WICBitmapLockRead.0 as u32)?;
            let stride = lock.GetStride()? as usize;
            let (mut length, mut data) = (0u32, std::ptr::null_mut());
            lock.GetDataPointer(&mut length, &mut data)?;
            if data.is_null() {
                return Ok(None);
            }
            let bytes = std::slice::from_raw_parts(data, length as usize);
            let row = width as usize * 4;
            let mut pixels = Vec::with_capacity(row * height as usize);
            for line in bytes.chunks(stride).take(height as usize) {
                let Some(line) = line.get(..row) else {
                    return Ok(None);
                };
                pixels.extend_from_slice(line);
            }
            drop(lock);
            // The engine leaves alpha undefined in BGRA output; GPUI blends
            // with it, so the picture is made opaque.
            for pixel in pixels.as_chunks_mut::<4>().0 {
                pixel[3] = 255;
            }
            let Some(image) = RgbaImage::from_raw(width, height, pixels) else {
                return Ok(None);
            };
            Ok(Some(Arc::new(RenderImage::new(SmallVec::from_elem(
                ImageFrame::new(image),
                1,
            )))))
        }
    }
}

/// Maps the engine's error code, and the HRESULT behind it, to a kind.
/// How far short of its stated length a file may end before it counts as
/// cut off. Stated lengths and the last frame disagree by a frame or two.
const EARLY_END_SECONDS: f64 = 1.0;

/// The shortest gap in the picture, while the clock runs and the window
/// draws, that counts as the media having run out.
const STALL_SECONDS: f64 = 4.0;

/// Whether to print each engine event, for finding out what the engine does
/// with a file (`GPUI_AV_MEDIAFOUNDATION_TRACE=1`).
fn trace() -> bool {
    static TRACE: OnceLock<bool> = OnceLock::new();
    *TRACE.get_or_init(|| std::env::var_os("GPUI_AV_MEDIAFOUNDATION_TRACE").is_some())
}

/// A stream attribute holding a GUID.
fn stream_guid(
    streams: &IMFMediaEngineEx,
    stream: u32,
    key: &windows::core::GUID,
) -> Option<windows::core::GUID> {
    with_stream_attribute(streams, stream, key, |value| {
        // SAFETY: reading the union member the tag names.
        unsafe {
            let value = &value.Anonymous.Anonymous;
            (value.vt == VT_CLSID && !value.Anonymous.puuid.is_null())
                .then(|| *value.Anonymous.puuid)
        }
    })
}

/// A stream attribute holding a string.
fn stream_string(
    streams: &IMFMediaEngineEx,
    stream: u32,
    key: &windows::core::GUID,
) -> Option<String> {
    with_stream_attribute(streams, stream, key, |value| {
        // SAFETY: reading the union member the tag names.
        unsafe {
            let value = &value.Anonymous.Anonymous;
            if value.vt != VT_LPWSTR || value.Anonymous.pwszVal.is_null() {
                return None;
            }
            value.Anonymous.pwszVal.to_string().ok()
        }
    })
    .filter(|text| !text.is_empty())
}

fn with_stream_attribute<T>(
    streams: &IMFMediaEngineEx,
    stream: u32,
    key: &windows::core::GUID,
    read: impl FnOnce(&PROPVARIANT) -> Option<T>,
) -> Option<T> {
    // SAFETY: the engine fills a PROPVARIANT we own and clear.
    unsafe {
        let mut value = streams.GetStreamAttribute(stream, key).ok()?;
        let result = read(&value);
        if let Err(error) = PropVariantClear(&mut value) {
            log(&format!("could not free a stream attribute: {error}"));
        }
        result
    }
}

fn error(code: usize, hresult: HRESULT) -> Error {
    const FILE_NOT_FOUND: u32 = 0x8007_0002;
    const PATH_NOT_FOUND: u32 = 0x8007_0003;
    const HTTP_DENIED: u32 = 0x8019_0191;
    const HTTP_FORBIDDEN: u32 = 0x8019_0193;
    const HTTP_NOT_FOUND: u32 = 0x8019_0194;
    const HTTP_GONE: u32 = 0x8019_019a;
    let kind = match hresult.0 as u32 {
        FILE_NOT_FOUND | PATH_NOT_FOUND | HTTP_NOT_FOUND | HTTP_GONE => ErrorKind::NotFound,
        HTTP_DENIED | HTTP_FORBIDDEN => ErrorKind::Unauthorized,
        // Other HTTP statuses.
        status if status & 0xffff_0000 == 0x8019_0000 => ErrorKind::Network,
        _ => match code as i32 {
            code if code == MF_MEDIA_ENGINE_ERR_NETWORK.0 => ErrorKind::Network,
            code if code == MF_MEDIA_ENGINE_ERR_DECODE.0 => ErrorKind::Decode,
            code if code == MF_MEDIA_ENGINE_ERR_SRC_NOT_SUPPORTED.0 => ErrorKind::Unsupported,
            code if code == MF_MEDIA_ENGINE_ERR_ENCRYPTED.0 => ErrorKind::Unsupported,
            code if code == MF_MEDIA_ENGINE_ERR_ABORTED.0 => ErrorKind::Other,
            _ => ErrorKind::Other,
        },
    };
    let message = windows::core::Error::from(hresult).message();
    Error::new(
        kind,
        format!(
            "media engine error {code} ({:#010x}: {message})",
            hresult.0 as u32
        ),
    )
}

/// A string Media Foundation allocated for the caller, copied out and
/// freed. Empty strings count as none.
///
/// # Safety
///
/// `string` must be null or a string from `CoTaskMemAlloc` that nothing
/// else frees.
unsafe fn owned_string(string: windows::core::Result<PWSTR>) -> Option<String> {
    let string = string.ok()?;
    if string.is_null() {
        return None;
    }
    // SAFETY: a valid null-terminated string, per the contract above.
    let text = unsafe { string.to_string() }.ok();
    // SAFETY: the caller owns it, per the contract above.
    unsafe { CoTaskMemFree(Some(string.0 as *const _)) };
    text.filter(|text| !text.is_empty())
}

/// Whether a URL names an HLS playlist.
fn names_playlist(url: &str) -> bool {
    url.split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase()
        .ends_with(".m3u8")
}

/// The error for what the HLS proxy's server answered.
fn proxy_error(failure: gpui_av_remux::Failure) -> Error {
    let kind = match failure.kind {
        gpui_av_remux::FailureKind::Io(std::io::ErrorKind::NotFound) => ErrorKind::NotFound,
        gpui_av_remux::FailureKind::Io(std::io::ErrorKind::PermissionDenied) => {
            ErrorKind::Unauthorized
        }
        _ => ErrorKind::Network,
    };
    Error::new(kind, failure.message)
}

pub(crate) fn log(message: &str) {
    eprintln!("gpui-av-mediafoundation: {message}");
}

impl Session for MfSession {
    fn select_track(&mut self, kind: TrackKind, index: Option<usize>) {
        if kind == TrackKind::Text {
            let mut inner = self.inner.borrow_mut();
            if !inner.loaded {
                inner.pending_text = Some(index);
                return;
            }
            inner.select_text(index);
            inner.report_info();
            return;
        }
        let (TrackKind::Audio, Some(index)) = (kind, index) else {
            return;
        };
        let mut inner = self.inner.borrow_mut();
        if !inner.loaded {
            inner.pending_audio = Some(index);
            return;
        }
        inner.select_audio(index);
        inner.report_info();
    }

    fn play(&mut self) {
        let mut inner = self.inner.borrow_mut();
        if inner.waiting_for_source {
            inner.wanted_playing = Some(true);
            return;
        }
        // SAFETY: a plain engine command on the main thread.
        if let Err(error) = unsafe { inner.engine.Play() } {
            log(&format!("play failed: {error}"));
        }
    }

    fn pause(&mut self) {
        let mut inner = self.inner.borrow_mut();
        if inner.waiting_for_source {
            inner.wanted_playing = Some(false);
            return;
        }
        // SAFETY: as above.
        if let Err(error) = unsafe { inner.engine.Pause() } {
            log(&format!("pause failed: {error}"));
        }
    }

    fn seek(&mut self, position: Duration) {
        self.inner.borrow_mut().seek(position);
    }

    fn set_volume(&mut self, volume: f32) {
        // SAFETY: as above.
        if let Err(error) = unsafe {
            self.inner
                .borrow()
                .engine
                .SetVolume(f64::from(volume.clamp(0.0, 1.0)))
        } {
            log(&format!("volume failed: {error}"));
        }
    }

    fn set_muted(&mut self, muted: bool) {
        // SAFETY: as above.
        if let Err(error) = unsafe { self.inner.borrow().engine.SetMuted(muted) } {
            log(&format!("mute failed: {error}"));
        }
    }

    fn set_looping(&mut self, looping: bool) {
        // SAFETY: as above.
        if let Err(error) = unsafe { self.inner.borrow().engine.SetLoop(looping) } {
            log(&format!("loop failed: {error}"));
        }
    }

    fn text(&self) -> Option<SharedString> {
        let inner = self.inner.borrow();
        if !inner.loaded {
            return None;
        }
        inner.current_text().map(SharedString::from)
    }

    fn position(&self) -> Duration {
        let inner = self.inner.borrow();
        if !inner.loaded {
            return inner.pending_seek.unwrap_or_default();
        }
        // SAFETY: a plain engine query on the main thread.
        seconds(unsafe { inner.engine.GetCurrentTime() }).unwrap_or_default()
    }

    fn buffered(&self) -> Vec<Range<Duration>> {
        let inner = self.inner.borrow();
        if !inner.loaded {
            return Vec::new();
        }
        // SAFETY: plain engine queries on the main thread, for indices the
        // range list says it has.
        unsafe {
            let Ok(ranges) = inner.engine.GetBuffered() else {
                return Vec::new();
            };
            (0..ranges.GetLength())
                .filter_map(|index| {
                    let start = seconds(ranges.GetStart(index).ok()?)?;
                    let end = seconds(ranges.GetEnd(index).ok()?)?;
                    (start < end).then_some(start..end)
                })
                .collect()
        }
    }

    fn stats(&self) -> Stats {
        // The engine does not say which frames it skipped.
        Stats::new(self.inner.borrow().presented, 0)
    }

    fn paint(&mut self, placement: Placement, window: &mut Window, _cx: &mut App) {
        let mut inner = self.inner.borrow_mut();
        if inner.failed {
            return;
        }
        inner.last_paint = Some(Instant::now());
        inner.take_frame();
        for image in inner.retired.drain(..) {
            // Frees the old frame's texture in GPUI's atlas.
            if let Err(error) = window.drop_image(image) {
                log(&format!("could not free a frame: {error}"));
            }
        }
        if let Some(image) = inner.shown.clone()
            && let Err(error) = window.paint_image(
                placement.bounds,
                placement.bounds,
                Corners::default(),
                image,
                0,
                false,
            )
        {
            log(&format!("could not draw a frame: {error}"));
        }
    }
}

impl Drop for MfSession {
    fn drop(&mut self) {
        // SAFETY: Shutdown stops the engine's threads and releases the
        // source; the engine is not used afterwards.
        if let Err(error) = unsafe { self.inner.borrow().engine.Shutdown() } {
            log(&format!("shutdown failed: {error}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FrameRun;

    fn run(rate: f64, count: u32) -> FrameRun {
        let mut run = FrameRun::default();
        for frame in 0..count {
            run.push(f64::from(frame) / rate);
        }
        run
    }

    #[test]
    fn a_picture_that_stops_while_the_clock_runs_is_a_stall() {
        let video = run(30.0, 90);
        assert!(!video.stalled_at(3.5));
        assert!(video.stalled_at(8.0));
    }

    #[test]
    fn slow_pictures_and_cover_images_are_not_stalls() {
        assert!(!run(30.0, 1).stalled_at(60.0));
        // A slideshow, a picture every 5 s.
        assert!(!run(0.2, 12).stalled_at(55.0 + 30.0));
        assert!(run(0.2, 12).stalled_at(55.0 + 60.0));
    }
}
