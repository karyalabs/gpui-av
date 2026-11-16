//! A `playbin` whose video ends in an `appsink` that hands out BGRA
//! frames as GPUI images, and the safe calls the session makes on it.

use std::ffi::{CString, c_char, c_int, c_uint, c_void};
use std::ops::Range;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::RenderImage;
use gpui_av::{
    ColorInfo, ColorMatrix, ColorRange, Error, ErrorKind, Rotation, Track, TrackKind,
    TransferFunction,
};
use image::{Frame as ImageFrame, RgbaImage};
use smallvec::SmallVec;

use crate::ffi::{self, Api, GstMessage};

/// What the video branch does to each decoded frame: turn it upright
/// (rotation tags), convert it to the BGRA GPUI's images hold, and hand it
/// to us. `n-threads=0` lets the conversion use every core, which keeps
/// the time from decode to screen short.
#[cfg(not(feature = "karya-gpui"))]
const VIDEO_SINK: &str = "videoflip video-direction=auto ! videoconvert n-threads=0 \
     ! appsink name=sink caps=video/x-raw,format=BGRA sync=true qos=true \
     max-lateness=20000000 enable-last-sample=false max-buffers=1 drop=true";

/// With Karya Labs' GPUI the renderer converts YUV itself (spec 4.3, patch
/// 4), so frames stay NV12, which most decoders give anyway (the converter
/// then passes them through), and reach GPUI with no copy at all.
#[cfg(feature = "karya-gpui")]
const VIDEO_SINK: &str = "videoflip video-direction=auto ! videoconvert n-threads=0 \
     ! appsink name=sink caps=video/x-raw,format=NV12 sync=true qos=true \
     max-lateness=20000000 enable-last-sample=false max-buffers=1 drop=true";

/// With `GPUI_AV_DMABUF=1` (an experiment, E5 step 2): frames stay in the
/// hardware decoder's memory and reach GPUI as dmabufs. No converter, so no
/// rotation and no software decoders: only a decoder that outputs dmabufs
/// (GStreamer's VA-API decoders) can link.
#[cfg(all(feature = "karya-gpui", target_os = "linux"))]
const VIDEO_SINK_DMABUF: &str = "appsink name=sink caps=video/x-raw(memory:DMABuf),format=DMA_DRM \
     sync=true qos=true max-lateness=20000000 enable-last-sample=false max-buffers=1 drop=true";

/// The video branch for this process: see `VIDEO_SINK_DMABUF`.
#[cfg(all(feature = "karya-gpui", target_os = "linux"))]
fn video_sink(api: &Api) -> &'static str {
    if api.dmabuf.is_some() && std::env::var_os("GPUI_AV_DMABUF").is_some_and(|value| value == "1")
    {
        VIDEO_SINK_DMABUF
    } else {
        VIDEO_SINK
    }
}

#[cfg(not(all(feature = "karya-gpui", target_os = "linux")))]
fn video_sink(_api: &Api) -> &'static str {
    VIDEO_SINK
}

/// A decoded frame, ready to paint.
pub(crate) struct Frame {
    /// The presentation time, when the stream gave one.
    pub pts: Option<Duration>,
    pub picture: Picture,
}

/// A frame's pixels in the form GPUI draws them.
#[derive(Clone)]
pub(crate) enum Picture {
    /// BGRA, copied into an image (stock GPUI).
    #[cfg_attr(feature = "karya-gpui", allow(dead_code))]
    Image(Arc<RenderImage>),
    /// The decoder's own NV12 buffer, drawn by Karya Labs' GPUI.
    #[cfg(feature = "karya-gpui")]
    Video(Arc<gpui::VideoFrame>),
}

/// What the streaming thread leaves for the main thread.
#[derive(Default)]
pub(crate) struct Frames {
    /// The newest frame not yet painted.
    pub latest: Option<Frame>,
    /// Frames replaced before a paint took them.
    pub replaced: u64,
}

/// Shared between the appsink's callbacks and the session.
#[derive(Default)]
pub(crate) struct Shared {
    pub frames: Mutex<Frames>,
    /// Time spent copying frames out of GStreamer, and how many, for the
    /// copy cost in E2.
    pub copy_nanos: AtomicU64,
    pub copies: AtomicU64,
    /// The presentation time of the newest frame, in nanoseconds plus one
    /// (0: none yet).
    last_pts: AtomicU64,
    /// The video branch reached its end.
    pub video_ended: AtomicBool,
}

impl Shared {
    /// The presentation time of the newest frame the sink took.
    pub fn last_pts(&self) -> Option<Duration> {
        match self.last_pts.load(Ordering::Acquire) {
            0 => None,
            nanos => Some(Duration::from_nanos(nanos - 1)),
        }
    }
}

/// One GStreamer object reference, released on drop.
struct Object(*mut c_void);

impl Drop for Object {
    fn drop(&mut self) {
        if let Ok(api) = ffi::api() {
            // SAFETY: we own one reference.
            unsafe { (api.gst.gst_object_unref)(self.0) };
        }
    }
}

/// A message popped from the bus, released on drop.
pub(crate) struct Message<'a> {
    api: &'a Api,
    raw: *mut GstMessage,
}

impl Drop for Message<'_> {
    fn drop(&mut self) {
        // SAFETY: we own the message.
        unsafe { (self.api.gst.gst_mini_object_unref)(self.raw.cast()) };
    }
}

/// What a bus message means to the session.
pub(crate) enum Event {
    Eos,
    Error(Error),
    /// The orientation tag of the video.
    Rotation(Rotation),
    Buffering(i32),
    /// The pipeline itself reached this state.
    State(c_int),
    AsyncDone,
    DurationChanged,
    ClockLost,
    /// GStreamer lacks a plugin for part of the media: its name, and
    /// whether the part is the picture or the sound.
    MissingPlugin {
        name: String,
        essential: bool,
    },
    /// The video sink's count of frames dropped for being late.
    SinkDropped(u64),
    Other,
}

pub(crate) struct Pipeline {
    api: &'static Api,
    playbin: *mut c_void,
    bus: *mut c_void,
    sink: Object,
    pub shared: Arc<Shared>,
    header_setup: Option<Arc<HeaderSetup>>,
    /// A local file: all of it is there to play.
    local: bool,
    /// Where playbin sends the selected text track.
    text_sink: Option<Object>,
    /// The colour put in place of the decoder's, when the bin sets one.
    colour: Option<ColorInfo>,
}

/// The request headers, and whether the source playbin picked could not
/// take them.
struct HeaderSetup {
    headers: Vec<(CString, CString)>,
    refused: AtomicBool,
}

impl Pipeline {
    /// Builds the pipeline for `uri` without starting it. `headers` go
    /// with every HTTP request.
    /// `colorimetry` (GStreamer's text for it), when given, replaces what
    /// the decoder says, before the frames are converted.
    pub fn new(
        api: &'static Api,
        uri: &str,
        headers: &[(String, String)],
        colorimetry: Option<&str>,
    ) -> Result<Self, Error> {
        let local = uri.starts_with("file:");
        let uri = c_string(uri)?;
        let headers = headers
            .iter()
            .map(|(name, value)| Ok((c_string(name)?, c_string(value)?)))
            .collect::<Result<Vec<_>, Error>>()?;
        let gst = &api.gst;
        let gobject = &api.gobject;
        // SAFETY: plain GStreamer construction calls with valid strings; every
        // pointer is checked before use, and each reference has one owner.
        unsafe {
            let playbin = (gst.gst_element_factory_make)(c"playbin".as_ptr(), ptr::null());
            if playbin.is_null() {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    "GStreamer has no playbin: install its base plugins",
                ));
            }
            let playbin = (gst.gst_object_ref_sink)(playbin);
            let playbin_ref = Object(playbin);

            let mut error = ptr::null_mut();
            // capssetter is in plugins-good; without it, the decoder's
            // colorimetry stands.
            let mut colour = None;
            let bin = colorimetry
                .and_then(|colorimetry| {
                    let description = c_string(&format!(
                        "capssetter caps=\"video/x-raw,colorimetry=(string){colorimetry}\" ! {}",
                        video_sink(api)
                    ))
                    .ok()?;
                    let mut ignored = ptr::null_mut();
                    let bin =
                        (gst.gst_parse_bin_from_description)(description.as_ptr(), 1, &mut ignored);
                    ffi::take_error(&api.glib, ignored);
                    colour = (!bin.is_null()).then(|| parse_colorimetry(colorimetry));
                    (!bin.is_null()).then_some(bin)
                })
                .unwrap_or(ptr::null_mut());
            let bin = if bin.is_null() {
                let description = c_string(video_sink(api))?;
                (gst.gst_parse_bin_from_description)(description.as_ptr(), 1, &mut error)
            } else {
                bin
            };
            if bin.is_null() {
                let message = ffi::take_error(&api.glib, error)
                    .map(|(_, _, message)| message)
                    .unwrap_or_default();
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    format!("GStreamer cannot build the video branch: {message}"),
                ));
            }
            let sink = Object((gst.gst_bin_get_by_name)(bin, c"sink".as_ptr()));
            // Takes the bin's floating reference.
            (gobject.g_object_set)(playbin, c"video-sink".as_ptr(), bin, ptr::null::<c_void>());
            let mut flags: c_uint = 0;
            (gobject.g_object_get)(
                playbin,
                c"flags".as_ptr(),
                &mut flags,
                ptr::null::<c_void>(),
            );
            (gobject.g_object_set)(
                playbin,
                c"flags".as_ptr(),
                // Text goes to our own sink (below), never burned into
                // the picture; the session shows it only once chosen.
                flags | ffi::GST_PLAY_FLAG_NATIVE_VIDEO | ffi::GST_PLAY_FLAG_TEXT,
                ptr::null::<c_void>(),
            );
            let text_sink = (gst.gst_element_factory_make)(c"appsink".as_ptr(), ptr::null());
            let text_sink = (!text_sink.is_null()).then(|| {
                let text_sink = Object((gst.gst_object_ref_sink)(text_sink));
                // Keep only the latest few cues if nobody reads them.
                (gobject.g_object_set)(
                    text_sink.0,
                    c"max-buffers".as_ptr(),
                    8 as c_uint,
                    c"drop".as_ptr(),
                    1 as c_int,
                    ptr::null::<c_void>(),
                );
                (gobject.g_object_set)(
                    playbin,
                    c"text-sink".as_ptr(),
                    text_sink.0,
                    ptr::null::<c_void>(),
                );
                text_sink
            });
            (gobject.g_object_set)(
                playbin,
                c"uri".as_ptr(),
                uri.as_ptr(),
                ptr::null::<c_void>(),
            );

            let header_setup = (!headers.is_empty()).then(|| {
                Arc::new(HeaderSetup {
                    headers,
                    refused: AtomicBool::new(false),
                })
            });
            if let Some(setup) = &header_setup {
                let headers = Arc::into_raw(setup.clone());
                (gobject.g_signal_connect_data)(
                    playbin,
                    c"source-setup".as_ptr(),
                    on_source_setup as *mut c_void,
                    headers as *mut c_void,
                    Some(release_headers),
                    0,
                );
            }

            let shared = Arc::new(Shared::default());
            let callbacks = ffi::GstAppSinkCallbacks {
                eos: Some(on_eos),
                new_preroll: Some(on_preroll),
                new_sample: Some(on_sample),
                reserved: [ptr::null_mut(); 4],
            };
            (api.app.gst_app_sink_set_callbacks)(
                sink.0,
                &callbacks,
                Arc::into_raw(shared.clone()) as *mut c_void,
                Some(release_shared),
            );

            let bus = (gst.gst_element_get_bus)(playbin);
            std::mem::forget(playbin_ref);
            Ok(Self {
                api,
                playbin,
                bus,
                sink,
                shared,
                header_setup,
                local,
                text_sink,
                colour,
            })
        }
    }

    /// Whether the source playbin chose for the URL cannot send request
    /// headers, so the media must not play.
    pub fn headers_refused(&self) -> bool {
        self.header_setup
            .as_ref()
            .is_some_and(|setup| setup.refused.load(Ordering::Acquire))
    }

    /// Asks for a state; returns false when GStreamer refused at once.
    pub fn set_state(&self, state: c_int) -> bool {
        // SAFETY: a valid element.
        let result = unsafe { (self.api.gst.gst_element_set_state)(self.playbin, state) };
        result != ffi::GST_STATE_CHANGE_FAILURE
    }

    pub fn seek(&self, position: Duration) -> bool {
        let nanos = i64::try_from(position.as_nanos()).unwrap_or(i64::MAX);
        // SAFETY: a valid element.
        unsafe {
            (self.api.gst.gst_element_seek_simple)(
                self.playbin,
                ffi::GST_FORMAT_TIME,
                ffi::GST_SEEK_FLAG_FLUSH | ffi::GST_SEEK_FLAG_ACCURATE,
                nanos,
            ) != 0
        }
    }

    pub fn position(&self) -> Option<Duration> {
        let mut nanos = 0i64;
        // SAFETY: a valid element and out parameter.
        let known = unsafe {
            (self.api.gst.gst_element_query_position)(
                self.playbin,
                ffi::GST_FORMAT_TIME,
                &mut nanos,
            )
        };
        (known != 0 && nanos >= 0).then(|| Duration::from_nanos(nanos as u64))
    }

    pub fn duration(&self) -> Option<Duration> {
        let mut nanos = 0i64;
        // SAFETY: a valid element and out parameter.
        let known = unsafe {
            (self.api.gst.gst_element_query_duration)(
                self.playbin,
                ffi::GST_FORMAT_TIME,
                &mut nanos,
            )
        };
        (known != 0 && nanos >= 0).then(|| Duration::from_nanos(nanos as u64))
    }

    /// The stretches that can play without waiting for the network: all
    /// of a local file, otherwise what playbin's buffering query reports
    /// (time ranges, or percentages of `duration`).
    pub fn buffered(&self) -> Vec<Range<Duration>> {
        let Some(duration) = self.duration() else {
            return Vec::new();
        };
        if self.local {
            return vec![Duration::ZERO..duration];
        }
        let gst = &self.api.gst;
        // SAFETY: a new query we own and free; a valid element; plain out
        // parameters, read only for indices the query says it has.
        unsafe {
            let query = (gst.gst_query_new_buffering)(ffi::GST_FORMAT_PERCENT);
            if query.is_null() {
                return Vec::new();
            }
            let mut ranges = Vec::new();
            if (gst.gst_element_query)(self.playbin, query) != 0 {
                let mut format = 0;
                let (mut start, mut stop, mut total) = (0i64, 0i64, 0i64);
                (gst.gst_query_parse_buffering_range)(
                    query,
                    &mut format,
                    &mut start,
                    &mut stop,
                    &mut total,
                );
                for index in 0..(gst.gst_query_get_n_buffering_ranges)(query) {
                    let (mut start, mut stop) = (0i64, 0i64);
                    if (gst.gst_query_parse_nth_buffering_range)(
                        query, index, &mut start, &mut stop,
                    ) != 0
                        && let Some(range) = to_time(format, start, stop, duration)
                    {
                        ranges.push(range);
                    }
                }
                // An element that keeps one range reports it only here.
                if ranges.is_empty()
                    && let Some(range) = to_time(format, start, stop, duration)
                {
                    ranges.push(range);
                }
            }
            (gst.gst_mini_object_unref)(query);
            ranges
        }
    }

    pub fn set_volume(&self, volume: f32) {
        let volume = f64::from(volume.clamp(0.0, 1.0));
        // SAFETY: playbin's `volume` is a gdouble.
        unsafe {
            (self.api.gobject.g_object_set)(
                self.playbin,
                c"volume".as_ptr(),
                volume,
                ptr::null::<c_void>(),
            )
        };
    }

    pub fn set_muted(&self, muted: bool) {
        // SAFETY: playbin's `mute` is a gboolean.
        unsafe {
            (self.api.gobject.g_object_set)(
                self.playbin,
                c"mute".as_ptr(),
                c_int::from(muted),
                ptr::null::<c_void>(),
            )
        };
    }

    fn int_property(&self, name: &std::ffi::CStr) -> i32 {
        let mut value: c_int = 0;
        // SAFETY: `name` is an int property of playbin.
        unsafe {
            (self.api.gobject.g_object_get)(
                self.playbin,
                name.as_ptr(),
                &mut value,
                ptr::null::<c_void>(),
            )
        };
        value
    }

    pub fn video_streams(&self) -> i32 {
        self.int_property(c"n-video")
    }

    pub fn audio_streams(&self) -> i32 {
        self.int_property(c"n-audio")
    }

    fn set_int_property(&self, name: &std::ffi::CStr, value: c_int) {
        // SAFETY: `name` is an int property of playbin.
        unsafe {
            (self.api.gobject.g_object_set)(
                self.playbin,
                name.as_ptr(),
                value,
                ptr::null::<c_void>(),
            )
        };
    }

    /// The audio and text tracks, with the language and title tags
    /// playbin collected for each.
    pub fn tracks(&self) -> Vec<Track> {
        let mut tracks = Vec::new();
        for (kind, count, signal) in [
            (TrackKind::Audio, c"n-audio", c"get-audio-tags"),
            (TrackKind::Text, c"n-text", c"get-text-tags"),
        ] {
            for index in 0..self.int_property(count).max(0) {
                let mut track = Track::new(kind, index as usize);
                let mut list: *mut c_void = ptr::null_mut();
                // SAFETY: the signal returns a tag list we own, or null;
                // `tag` releases it.
                unsafe {
                    (self.api.gobject.g_signal_emit_by_name)(
                        self.playbin,
                        signal.as_ptr(),
                        index,
                        &mut list,
                    );
                    if !list.is_null() {
                        track.language = self.tag(list, c"language-code");
                        track.title = self.tag(list, c"title");
                        (self.api.gst.gst_mini_object_unref)(list);
                    }
                }
                tracks.push(track);
            }
        }
        tracks
    }

    /// # Safety
    /// `list` must be a valid tag list.
    unsafe fn tag(&self, list: *mut c_void, name: &std::ffi::CStr) -> Option<String> {
        // SAFETY: per the contract; the string is ours to free.
        unsafe {
            let mut value: *mut c_char = ptr::null_mut();
            let found =
                (self.api.gst.gst_tag_list_get_string)(list, name.as_ptr(), &mut value) != 0;
            let text = ffi::string(value);
            (self.api.glib.g_free)(value.cast());
            text.filter(|text| found && !text.is_empty())
        }
    }

    /// The audio track playing, when there is one.
    pub fn audio_track(&self) -> Option<usize> {
        usize::try_from(self.int_property(c"current-audio")).ok()
    }

    /// The text track playbin sends to the text sink.
    pub fn text_track(&self) -> Option<usize> {
        usize::try_from(self.int_property(c"current-text")).ok()
    }

    /// The next cue the text sink has, if any: its text (markup removed),
    /// start and end.
    pub fn take_cue(&self) -> Option<(String, Option<Duration>, Option<Duration>)> {
        let sink = self.text_sink.as_ref()?;
        let gst = &self.api.gst;
        // SAFETY: a valid appsink; the sample and its buffer are ours until
        // unreferenced, and the mapped bytes are read only while mapped.
        unsafe {
            let sample = (self.api.app.gst_app_sink_try_pull_sample)(sink.0, 0);
            if sample.is_null() {
                return None;
            }
            let buffer = (gst.gst_sample_get_buffer)(sample);
            let mut cue = None;
            if !buffer.is_null() {
                let time = |value: u64| {
                    (value != ffi::GST_CLOCK_TIME_NONE).then(|| Duration::from_nanos(value))
                };
                let start = time((*buffer).pts);
                let end = start
                    .zip(time((*buffer).duration))
                    .map(|(start, length)| start + length);
                let mut map = std::mem::zeroed::<ffi::GstMapInfo>();
                if (gst.gst_buffer_map)(buffer, &mut map, ffi::GST_MAP_READ) != 0 {
                    let bytes = std::slice::from_raw_parts(map.data, map.size);
                    let text = strip_markup(&String::from_utf8_lossy(bytes));
                    (gst.gst_buffer_unmap)(buffer, &mut map);
                    cue = Some((text, start, end));
                }
            }
            (gst.gst_mini_object_unref)(sample);
            cue
        }
    }

    /// Applies a track choice. Turning text off is the session's part (it
    /// stops showing cues); an audio track cannot be turned off.
    pub fn select_track(&self, kind: TrackKind, index: Option<usize>) {
        match (kind, index) {
            (TrackKind::Audio, Some(index)) => self.set_audio_track(index),
            (TrackKind::Text, Some(index)) => self.set_text_track(index),
            _ => {}
        }
    }

    fn set_audio_track(&self, index: usize) {
        if let Ok(index) = c_int::try_from(index)
            && index < self.int_property(c"n-audio")
        {
            self.set_int_property(c"current-audio", index);
        }
    }

    fn set_text_track(&self, index: usize) {
        if let Ok(index) = c_int::try_from(index)
            && index < self.int_property(c"n-text")
        {
            self.set_int_property(c"current-text", index);
        }
    }

    /// The size of the frames the sink gets (upright), with the pixel
    /// aspect ratio applied.
    pub fn display_size(&self) -> Option<(u32, u32)> {
        // SAFETY: a valid element; the pad and caps references are ours.
        unsafe {
            let caps = self.pad_caps(self.sink.0, c"sink")?;
            self.caps_size(caps.raw)
        }
    }

    /// The decoder's colour description, from the caps playbin's first
    /// video stream arrives with.
    pub fn color(&self) -> ColorInfo {
        if let Some(colour) = self.colour {
            return colour;
        }
        // SAFETY: `get-video-pad` returns a pad reference we own (or null).
        unsafe {
            let mut pad: *mut c_void = ptr::null_mut();
            (self.api.gobject.g_signal_emit_by_name)(
                self.playbin,
                c"get-video-pad".as_ptr(),
                0 as c_int,
                &mut pad,
            );
            if pad.is_null() {
                return ColorInfo::default();
            }
            let pad = Object(pad);
            let caps = (self.api.gst.gst_pad_get_current_caps)(pad.0);
            if caps.is_null() {
                return ColorInfo::default();
            }
            let caps = Caps {
                api: self.api,
                raw: caps,
            };
            let structure = (self.api.gst.gst_caps_get_structure)(caps.raw, 0);
            let colorimetry = ffi::string((self.api.gst.gst_structure_get_string)(
                structure,
                c"colorimetry".as_ptr(),
            ));
            colorimetry
                .as_deref()
                .map(parse_colorimetry)
                .unwrap_or_default()
        }
    }

    /// The rotation tag of the first video stream, from the tags playbin
    /// collected for it.
    pub fn video_rotation(&self) -> Option<Rotation> {
        let mut list: *mut c_void = ptr::null_mut();
        // SAFETY: `get-video-tags` returns a tag list we own, or null.
        unsafe {
            (self.api.gobject.g_signal_emit_by_name)(
                self.playbin,
                c"get-video-tags".as_ptr(),
                0 as c_int,
                &mut list,
            );
            self.take_orientation(list)
        }
    }

    /// Reads the orientation tag from a tag list and releases the list.
    ///
    /// # Safety
    /// `list` must be null or a tag list the caller owns.
    unsafe fn take_orientation(&self, list: *mut c_void) -> Option<Rotation> {
        if list.is_null() {
            return None;
        }
        let api = self.api;
        // SAFETY: per the contract; the string is ours to free.
        unsafe {
            let mut value: *mut c_char = ptr::null_mut();
            let found =
                (api.gst.gst_tag_list_get_string)(list, c"image-orientation".as_ptr(), &mut value)
                    != 0;
            let orientation = ffi::string(value);
            (api.glib.g_free)(value.cast());
            (api.gst.gst_mini_object_unref)(list);
            orientation
                .filter(|_| found)
                .as_deref()
                .map(parse_orientation)
        }
    }

    /// # Safety
    /// `element` must be a valid element.
    unsafe fn pad_caps(&self, element: *mut c_void, name: &std::ffi::CStr) -> Option<Caps> {
        // SAFETY: per the contract; references are released by their owners.
        unsafe {
            let pad = (self.api.gst.gst_element_get_static_pad)(element, name.as_ptr());
            if pad.is_null() {
                return None;
            }
            let pad = Object(pad);
            let caps = (self.api.gst.gst_pad_get_current_caps)(pad.0);
            (!caps.is_null()).then(|| Caps {
                api: self.api,
                raw: caps,
            })
        }
    }

    /// # Safety
    /// `caps` must be valid caps.
    unsafe fn caps_size(&self, caps: *mut c_void) -> Option<(u32, u32)> {
        let gst = &self.api.gst;
        // SAFETY: per the contract.
        unsafe {
            let structure = (gst.gst_caps_get_structure)(caps, 0);
            let (mut width, mut height) = (0, 0);
            if (gst.gst_structure_get_int)(structure, c"width".as_ptr(), &mut width) == 0
                || (gst.gst_structure_get_int)(structure, c"height".as_ptr(), &mut height) == 0
            {
                return None;
            }
            let (mut numerator, mut denominator) = (1, 1);
            (gst.gst_structure_get_fraction)(
                structure,
                c"pixel-aspect-ratio".as_ptr(),
                &mut numerator,
                &mut denominator,
            );
            Some(display_size(width, height, numerator, denominator))
        }
    }

    /// The next message on the bus, if any.
    pub fn pop(&self) -> Option<Message<'static>> {
        // SAFETY: a valid bus; the message reference is ours.
        let raw = unsafe { (self.api.gst.gst_bus_pop)(self.bus) };
        (!raw.is_null()).then(|| Message { api: self.api, raw })
    }

    /// What a message means, for messages about this pipeline.
    pub fn read(&self, message: &Message) -> Event {
        let api = self.api;
        let gst = &api.gst;
        let raw = message.raw;
        // SAFETY: a valid message; every out value is released as GStreamer
        // documents.
        unsafe {
            let kind = (*raw).type_;
            let source = (*raw).src;
            match kind {
                ffi::GST_MESSAGE_EOS => Event::Eos,
                ffi::GST_MESSAGE_ERROR => {
                    let (mut error, mut debug) = (ptr::null_mut(), ptr::null_mut());
                    (gst.gst_message_parse_error)(raw, &mut error, &mut debug);
                    let debug_text = ffi::string(debug);
                    (api.glib.g_free)(debug.cast());
                    let (domain, code, text) = ffi::take_error(&api.glib, error)
                        .unwrap_or_else(|| (String::new(), 0, "GStreamer failed".into()));
                    Event::Error(crate::errors::map(
                        &domain,
                        code,
                        &text,
                        debug_text.as_deref(),
                    ))
                }
                ffi::GST_MESSAGE_TAG => {
                    let mut list = ptr::null_mut();
                    (gst.gst_message_parse_tag)(raw, &mut list);
                    match self.take_orientation(list) {
                        Some(rotation) => Event::Rotation(rotation),
                        None => Event::Other,
                    }
                }
                ffi::GST_MESSAGE_BUFFERING => {
                    let mut percent = 0;
                    (gst.gst_message_parse_buffering)(raw, &mut percent);
                    Event::Buffering(percent)
                }
                ffi::GST_MESSAGE_STATE_CHANGED if source == self.playbin => {
                    let (mut old, mut new, mut pending) = (0, 0, 0);
                    (gst.gst_message_parse_state_changed)(raw, &mut old, &mut new, &mut pending);
                    Event::State(new)
                }
                ffi::GST_MESSAGE_ASYNC_DONE if source == self.playbin => Event::AsyncDone,
                ffi::GST_MESSAGE_DURATION_CHANGED => Event::DurationChanged,
                ffi::GST_MESSAGE_CLOCK_LOST => Event::ClockLost,
                ffi::GST_MESSAGE_QOS if source == self.sink.0 => {
                    let (mut format, mut processed, mut dropped) = (0, 0u64, 0u64);
                    (gst.gst_message_parse_qos_stats)(
                        raw,
                        &mut format,
                        &mut processed,
                        &mut dropped,
                    );
                    if dropped == u64::MAX {
                        Event::Other
                    } else {
                        Event::SinkDropped(dropped)
                    }
                }
                ffi::GST_MESSAGE_ELEMENT => {
                    let structure = (gst.gst_message_get_structure)(raw);
                    if structure.is_null()
                        || ffi::string((gst.gst_structure_get_name)(structure)).as_deref()
                            != Some("missing-plugin")
                    {
                        return Event::Other;
                    }
                    let name =
                        ffi::string((gst.gst_structure_get_string)(structure, c"name".as_ptr()))
                            .unwrap_or_else(|| "a GStreamer plugin".into());
                    // The missing piece's caps are a GstCaps field; its
                    // text form is enough to tell a picture or sound
                    // decoder from a subtitle one.
                    let text = (gst.gst_structure_to_string)(structure);
                    let described = ffi::string(text).unwrap_or_default();
                    if !text.is_null() {
                        (api.glib.g_free)(text.cast());
                    }
                    Event::MissingPlugin {
                        name,
                        essential: missing_is_essential(&described),
                    }
                }
                _ => Event::Other,
            }
        }
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        // SAFETY: stopping the pipeline joins its streaming threads, after
        // which nothing calls back into us; then the references are dropped.
        unsafe {
            (self.api.gst.gst_element_set_state)(self.playbin, ffi::GST_STATE_NULL);
            (self.api.gst.gst_object_unref)(self.bus);
            (self.api.gst.gst_object_unref)(self.playbin);
        }
    }
}

/// Caps, released on drop.
struct Caps {
    api: &'static Api,
    raw: *mut c_void,
}

impl Drop for Caps {
    fn drop(&mut self) {
        // SAFETY: we own one reference.
        unsafe { (self.api.gst.gst_mini_object_unref)(self.raw) };
    }
}

fn c_string(value: &str) -> Result<CString, Error> {
    CString::new(value).map_err(|_| Error::new(ErrorKind::Unsupported, "a NUL byte in the source"))
}

/// The size a picture of `width` by `height` stored pixels shows at, given
/// its pixel aspect ratio.
pub(crate) fn display_size(
    width: i32,
    height: i32,
    numerator: i32,
    denominator: i32,
) -> (u32, u32) {
    let width = width.max(0) as u64;
    let height = height.max(0) as u64;
    if numerator <= 0 || denominator <= 0 || numerator == denominator {
        return (width as u32, height as u32);
    }
    let shown = (width * numerator as u64 + denominator as u64 / 2) / denominator as u64;
    (shown as u32, height as u32)
}

/// Reads GStreamer's `image-orientation` tag. A mirrored picture is
/// reported by its rotation alone.
pub(crate) fn parse_orientation(tag: &str) -> Rotation {
    match tag.trim_start_matches("flip-") {
        "rotate-90" => Rotation::Cw90,
        "rotate-180" => Rotation::Cw180,
        "rotate-270" => Rotation::Cw270,
        _ => Rotation::None,
    }
}

/// Whether a `missing-plugin` message, as text, is for a decoder of the
/// picture or the sound (playbin then plays the rest without it, silently),
/// not for subtitles or anything else.
pub(crate) fn missing_is_essential(described: &str) -> bool {
    described.contains("type=(string)decoder")
        && described
            .split_once("detail=(GstCaps)")
            .is_some_and(|(_, caps)| {
                // Quoted when the caps hold a comma.
                let caps = caps.trim_start_matches('"');
                caps.starts_with("video/") || caps.starts_with("audio/")
            })
}

/// Reads GStreamer's `colorimetry` caps field: one of its names, or
/// `range:matrix:transfer:primaries` as numbers.
pub(crate) fn parse_colorimetry(text: &str) -> ColorInfo {
    use ColorMatrix::*;
    use TransferFunction::*;
    let limited = Some(ColorRange::Limited);
    match text {
        "bt601" => ColorInfo {
            range: limited,
            matrix: Some(Bt601),
            transfer: Some(Sdr),
        },
        "bt709" => ColorInfo {
            range: limited,
            matrix: Some(Bt709),
            transfer: Some(Sdr),
        },
        "bt2020" | "bt2020-10" => ColorInfo {
            range: limited,
            matrix: Some(Bt2020),
            transfer: Some(Sdr),
        },
        "bt2100-pq" => ColorInfo {
            range: limited,
            matrix: Some(Bt2020),
            transfer: Some(Pq),
        },
        "bt2100-hlg" => ColorInfo {
            range: limited,
            matrix: Some(Bt2020),
            transfer: Some(Hlg),
        },
        "smpte240m" => ColorInfo {
            range: limited,
            matrix: None,
            transfer: Some(Sdr),
        },
        "sRGB" => ColorInfo {
            range: Some(ColorRange::Full),
            matrix: None,
            transfer: Some(Sdr),
        },
        _ => {
            let numbers: Vec<Option<u32>> = text.split(':').map(|part| part.parse().ok()).collect();
            let [range, matrix, transfer, ..] = numbers[..] else {
                return ColorInfo::default();
            };
            ColorInfo {
                // GstVideoColorRange
                range: match range {
                    Some(1) => Some(ColorRange::Full),
                    Some(2) => Some(ColorRange::Limited),
                    _ => None,
                },
                // GstVideoColorMatrix
                matrix: match matrix {
                    Some(3) => Some(Bt709),
                    Some(4) => Some(Bt601),
                    Some(6) => Some(Bt2020),
                    _ => None,
                },
                // GstVideoTransferFunction: 14 is SMPTE 2084, 15 ARIB STD-B67.
                transfer: match transfer {
                    None | Some(0) => None,
                    Some(14) => Some(Pq),
                    Some(15) => Some(Hlg),
                    Some(_) => Some(Sdr),
                },
            }
        }
    }
}

unsafe extern "C" fn on_source_setup(
    _playbin: *mut c_void,
    source: *mut c_void,
    data: *mut c_void,
) {
    let Ok(api) = ffi::api() else { return };
    // SAFETY: `data` is the header list this handler was connected with,
    // alive until `release_headers`; `source` is a valid element.
    unsafe {
        let setup = &*(data as *const HeaderSetup);
        // G_OBJECT_GET_CLASS: the class pointer is an instance's first field.
        let class = *(source as *const *mut c_void);
        if (api.gobject.g_object_class_find_property)(class, c"extra-headers".as_ptr()).is_null() {
            // The session fails the media rather than play it without them.
            setup.refused.store(true, Ordering::Release);
            return;
        }
        let structure = (api.gst.gst_structure_new_empty)(c"extra-headers".as_ptr());
        for (name, value) in &setup.headers {
            (api.gst.gst_structure_set)(
                structure,
                name.as_ptr(),
                G_TYPE_STRING,
                value.as_ptr(),
                ptr::null::<c_void>(),
            );
        }
        (api.gobject.g_object_set)(
            source,
            c"extra-headers".as_ptr(),
            structure,
            ptr::null::<c_void>(),
        );
        (api.gst.gst_structure_free)(structure);
    }
}

/// `G_TYPE_STRING`: fundamental type 16, shifted by two.
const G_TYPE_STRING: usize = 16 << 2;

unsafe extern "C" fn release_headers(data: *mut c_void) {
    // SAFETY: the pointer came from `Arc::into_raw` in `Pipeline::new`.
    drop(unsafe { Arc::from_raw(data as *const HeaderSetup) });
}

unsafe extern "C" fn release_shared(data: *mut c_void) {
    // SAFETY: the pointer came from `Arc::into_raw` in `Pipeline::new`.
    drop(unsafe { Arc::from_raw(data as *const Shared) });
}

unsafe extern "C" fn on_eos(_sink: *mut c_void, data: *mut c_void) {
    // SAFETY: called by appsink with the user data we gave it.
    let shared = unsafe { &*(data as *const Shared) };
    shared.video_ended.store(true, Ordering::Release);
}

unsafe extern "C" fn on_preroll(sink: *mut c_void, data: *mut c_void) -> ffi::GstFlowReturn {
    // SAFETY: called by appsink with the user data we gave it.
    unsafe { take_sample(sink, data, true) }
}

unsafe extern "C" fn on_sample(sink: *mut c_void, data: *mut c_void) -> ffi::GstFlowReturn {
    // SAFETY: called by appsink with the user data we gave it.
    unsafe { take_sample(sink, data, false) }
}

/// Copies the sink's next frame into a GPUI image: the one copy a frame
/// costs on its way to GPUI's texture upload.
///
/// # Safety
/// `sink` is our appsink and `data` our `Shared`.
unsafe fn take_sample(sink: *mut c_void, data: *mut c_void, preroll: bool) -> ffi::GstFlowReturn {
    let Ok(api) = ffi::api() else {
        return ffi::GST_FLOW_OK;
    };
    // SAFETY: per the contract; the sample reference is released below.
    unsafe {
        let shared = &*(data as *const Shared);
        let sample = if preroll {
            (api.app.gst_app_sink_pull_preroll)(sink)
        } else {
            (api.app.gst_app_sink_pull_sample)(sink)
        };
        if sample.is_null() {
            return ffi::GST_FLOW_OK;
        }
        let started = std::time::Instant::now();
        let frame = copy_frame(api, sample);
        (api.gst.gst_mini_object_unref)(sample);
        let Some(frame) = frame else {
            return ffi::GST_FLOW_OK;
        };
        shared
            .copy_nanos
            .fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
        shared.copies.fetch_add(1, Ordering::Relaxed);
        if let Some(pts) = frame.pts {
            shared
                .last_pts
                .store(pts.as_nanos() as u64 + 1, Ordering::Release);
        }
        let mut frames = shared
            .frames
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(old) = frames.latest.replace(frame) {
            // The preroll frame comes again as the first sample; that is
            // not a frame lost.
            if old.pts != frames.latest.as_ref().and_then(|frame| frame.pts) {
                frames.replaced += 1;
            }
        }
        ffi::GST_FLOW_OK
    }
}

/// # Safety
/// `sample` must be a valid sample.
unsafe fn copy_frame(api: &'static Api, sample: *mut c_void) -> Option<Frame> {
    let gst = &api.gst;
    // SAFETY: per the contract; the buffer is unmapped before returning.
    unsafe {
        let caps = (gst.gst_sample_get_caps)(sample);
        let buffer = (gst.gst_sample_get_buffer)(sample);
        if caps.is_null() || buffer.is_null() {
            return None;
        }
        let structure = (gst.gst_caps_get_structure)(caps, 0);
        let (mut width, mut height) = (0, 0);
        (gst.gst_structure_get_int)(structure, c"width".as_ptr(), &mut width);
        (gst.gst_structure_get_int)(structure, c"height".as_ptr(), &mut height);
        let (width, height) = (u32::try_from(width).ok()?, u32::try_from(height).ok()?);
        #[cfg(feature = "karya-gpui")]
        {
            let colorimetry = ffi::string((gst.gst_structure_get_string)(
                structure,
                c"colorimetry".as_ptr(),
            ));
            #[cfg(target_os = "linux")]
            if (gst.gst_caps_features_contains)(
                (gst.gst_caps_get_features)(caps, 0),
                c"memory:DMABuf".as_ptr(),
            ) != 0
            {
                let drm_format = ffi::string((gst.gst_structure_get_string)(
                    structure,
                    c"drm-format".as_ptr(),
                ));
                return dmabuf_frame(
                    api,
                    sample,
                    width,
                    height,
                    drm_format.as_deref(),
                    colorimetry.as_deref(),
                );
            }
            return video_frame(api, sample, width, height, colorimetry.as_deref());
        }
        #[cfg_attr(feature = "karya-gpui", allow(unreachable_code))]
        let row = width as usize * 4;
        let pts = (*buffer).pts;
        let mut map = ffi::GstMapInfo::default();
        if (gst.gst_buffer_map)(buffer, &mut map, ffi::GST_MAP_READ) == 0 {
            return None;
        }
        let bytes = std::slice::from_raw_parts(map.data, map.size);
        // The appsink takes no video meta, so rows are packed unless
        // GStreamer padded each to the same stride.
        let stride = if height > 0 {
            bytes.len() / height as usize
        } else {
            0
        };
        let pixels = if stride == row {
            bytes.get(..row * height as usize).map(<[u8]>::to_vec)
        } else if stride > row {
            let mut pixels = Vec::with_capacity(row * height as usize);
            for line in bytes.chunks_exact(stride).take(height as usize) {
                pixels.extend_from_slice(&line[..row]);
            }
            Some(pixels)
        } else {
            None
        };
        (gst.gst_buffer_unmap)(buffer, &mut map);
        let image = RgbaImage::from_raw(width, height, pixels?)?;
        Some(Frame {
            pts: (pts != ffi::GST_CLOCK_TIME_NONE).then(|| Duration::from_nanos(pts)),
            picture: Picture::Image(Arc::new(RenderImage::new(SmallVec::from_elem(
                ImageFrame::new(image),
                1,
            )))),
        })
    }
}

/// A sample whose buffer stays mapped for as long as GPUI needs its bytes.
#[cfg(feature = "karya-gpui")]
struct MappedSample {
    api: &'static Api,
    sample: *mut c_void,
    buffer: *mut ffi::GstBuffer,
    map: ffi::GstMapInfo,
}

// SAFETY: samples and buffers are reference counted with atomics, and the
// mapping is read-only, so any thread may read it or drop it.
#[cfg(feature = "karya-gpui")]
unsafe impl Send for MappedSample {}
#[cfg(feature = "karya-gpui")]
unsafe impl Sync for MappedSample {}

#[cfg(feature = "karya-gpui")]
impl AsRef<[u8]> for MappedSample {
    fn as_ref(&self) -> &[u8] {
        // SAFETY: mapped for reading until drop.
        unsafe { std::slice::from_raw_parts(self.map.data, self.map.size) }
    }
}

#[cfg(feature = "karya-gpui")]
impl Drop for MappedSample {
    fn drop(&mut self) {
        // SAFETY: we mapped the buffer and own the sample reference.
        unsafe {
            (self.api.gst.gst_buffer_unmap)(self.buffer, &mut self.map);
            (self.api.gst.gst_mini_object_unref)(self.sample);
        }
    }
}

/// Wraps an NV12 sample as a GPUI video frame without copying it. Takes a
/// reference to `sample` of its own.
///
/// # Safety
/// `sample` must be a valid sample with NV12 caps of `width` by `height`.
#[cfg(feature = "karya-gpui")]
unsafe fn video_frame(
    api: &'static Api,
    sample: *mut c_void,
    width: u32,
    height: u32,
    colorimetry: Option<&str>,
) -> Option<Frame> {
    let gst = &api.gst;
    // SAFETY: per the contract; the new reference is released by
    // `MappedSample`, or here when mapping fails.
    let mapped = unsafe {
        let buffer = (gst.gst_sample_get_buffer)(sample);
        if buffer.is_null() {
            return None;
        }
        (gst.gst_mini_object_ref)(sample);
        let mut map = ffi::GstMapInfo::default();
        if (gst.gst_buffer_map)(buffer, &mut map, ffi::GST_MAP_READ) == 0 {
            (gst.gst_mini_object_unref)(sample);
            return None;
        }
        MappedSample {
            api,
            sample,
            buffer,
            map,
        }
    };
    // SAFETY: a valid buffer, alive as long as `mapped`.
    let pts = unsafe { (*mapped.buffer).pts };
    // The appsink takes no video meta, so the layout is GStreamer's default
    // for NV12: rows padded to four bytes, chroma after the (even) luma rows.
    let stride = width.next_multiple_of(4);
    let chroma_offset = stride as usize * height.next_multiple_of(2) as usize;
    let colour = video_colour(colorimetry, height);
    let frame = gpui::VideoFrame::nv12(
        width,
        height,
        colour,
        mapped,
        (0, stride),
        (chroma_offset, stride),
    )
    .ok()?;
    Some(Frame {
        pts: (pts != ffi::GST_CLOCK_TIME_NONE).then(|| Duration::from_nanos(pts)),
        picture: Picture::Video(Arc::new(frame)),
    })
}

/// Keeps a sample, and so its decoder surface, alive.
#[cfg(all(feature = "karya-gpui", target_os = "linux"))]
struct SampleRef {
    api: &'static Api,
    sample: *mut c_void,
}

// SAFETY: samples are reference counted with atomics.
#[cfg(all(feature = "karya-gpui", target_os = "linux"))]
unsafe impl Send for SampleRef {}
#[cfg(all(feature = "karya-gpui", target_os = "linux"))]
unsafe impl Sync for SampleRef {}

#[cfg(all(feature = "karya-gpui", target_os = "linux"))]
impl Drop for SampleRef {
    fn drop(&mut self) {
        // SAFETY: we own this reference.
        unsafe { (self.api.gst.gst_mini_object_unref)(self.sample) };
    }
}

/// Wraps a DMA_DRM sample holding NV12 as a GPUI video frame without
/// touching its pixels. Takes a reference to `sample` of its own.
///
/// # Safety
/// `sample` must be a valid sample with DMABuf caps of `width` by `height`.
#[cfg(all(feature = "karya-gpui", target_os = "linux"))]
unsafe fn dmabuf_frame(
    api: &'static Api,
    sample: *mut c_void,
    width: u32,
    height: u32,
    drm_format: Option<&str>,
    colorimetry: Option<&str>,
) -> Option<Frame> {
    use std::os::fd::BorrowedFd;
    let dmabuf = api.dmabuf.as_ref()?;
    // "NV12" (linear) or "NV12:0x0300000000606010" (with a modifier).
    let (fourcc, modifier) = match drm_format?.split_once(':') {
        Some((fourcc, modifier)) => (
            fourcc,
            u64::from_str_radix(modifier.trim_start_matches("0x"), 16).ok()?,
        ),
        None => (drm_format?, 0),
    };
    if fourcc != "NV12" {
        log_once(&format!(
            "dmabuf frames in {fourcc} are not drawn (NV12 only)"
        ));
        return None;
    }
    let gst = &api.gst;
    // SAFETY: per the contract. The planes' descriptors are borrowed from
    // the buffer's memories and duplicated before the sample can go.
    unsafe {
        let buffer = (gst.gst_sample_get_buffer)(sample);
        if buffer.is_null() {
            return None;
        }
        let meta = (dmabuf.video.gst_buffer_get_video_meta)(buffer);
        if meta.is_null() || (*meta).n_planes < 2 {
            log_once("a dmabuf frame came without its plane layout");
            return None;
        }
        let plane = |index: usize| -> Option<gpui::DmabufPlane> {
            let (mut memory_index, mut length, mut skip) = (0, 0, 0);
            if (gst.gst_buffer_find_memory)(
                buffer,
                (*meta).offset[index],
                1,
                &mut memory_index,
                &mut length,
                &mut skip,
            ) == 0
            {
                return None;
            }
            let memory = (gst.gst_buffer_peek_memory)(buffer, memory_index);
            if memory.is_null() || (dmabuf.allocators.gst_is_dmabuf_memory)(memory) == 0 {
                return None;
            }
            let fd = (dmabuf.allocators.gst_dmabuf_memory_get_fd)(memory);
            if fd < 0 {
                return None;
            }
            let fd = BorrowedFd::borrow_raw(fd).try_clone_to_owned().ok()?;
            Some(gpui::DmabufPlane {
                fd,
                offset: u32::try_from((*memory).offset + skip).ok()?,
                stride: u32::try_from((*meta).stride[index]).ok()?,
            })
        };
        let (luma, chroma) = (plane(0)?, plane(1)?);
        let pts = (*buffer).pts;
        (gst.gst_mini_object_ref)(sample);
        let owner = SampleRef { api, sample };
        let frame = gpui::VideoFrame::nv12_dmabuf(
            width,
            height,
            video_colour(colorimetry, height),
            luma,
            chroma,
            modifier,
            owner,
        )
        .ok()?;
        Some(Frame {
            pts: (pts != ffi::GST_CLOCK_TIME_NONE).then(|| Duration::from_nanos(pts)),
            picture: Picture::Video(Arc::new(frame)),
        })
    }
}

#[cfg(all(feature = "karya-gpui", target_os = "linux"))]
fn log_once(message: &str) {
    static LOGGED: std::sync::Once = std::sync::Once::new();
    LOGGED.call_once(|| eprintln!("gpui-av-gstreamer: {message}"));
}

/// The YUV matrix and range GPUI converts with: the stream's, or what
/// players assume for its size when it states none (BT.601 below 720 rows).
#[cfg(feature = "karya-gpui")]
fn video_colour(colorimetry: Option<&str>, height: u32) -> gpui::VideoColour {
    let info = colorimetry.map(parse_colorimetry).unwrap_or_default();
    let matrix = match info.matrix {
        Some(ColorMatrix::Bt601) => gpui::VideoMatrix::Bt601,
        Some(ColorMatrix::Bt709) => gpui::VideoMatrix::Bt709,
        Some(ColorMatrix::Bt2020) => gpui::VideoMatrix::Bt2020,
        _ if height < 720 => gpui::VideoMatrix::Bt601,
        _ => gpui::VideoMatrix::Bt709,
    };
    gpui::VideoColour {
        matrix,
        full_range: info.range == Some(ColorRange::Full),
    }
}

/// A buffering range from a query, as times; `None` for a format this
/// cannot turn into time, or an empty range.
fn to_time(format: c_int, start: i64, stop: i64, duration: Duration) -> Option<Range<Duration>> {
    if start < 0 || stop <= start {
        return None;
    }
    let (start, stop) = match format {
        ffi::GST_FORMAT_TIME => (
            Duration::from_nanos(start as u64),
            Duration::from_nanos(stop as u64),
        ),
        ffi::GST_FORMAT_PERCENT => {
            let part = |value: i64| {
                duration.mul_f64(
                    value.min(ffi::GST_FORMAT_PERCENT_MAX) as f64
                        / ffi::GST_FORMAT_PERCENT_MAX as f64,
                )
            };
            (part(start), part(stop))
        }
        _ => return None,
    };
    let stop = stop.min(duration);
    (start < stop).then_some(start..stop)
}

/// Removes Pango markup (`<i>`, `<span ...>`) and its entities from a cue.
fn strip_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for char in text.chars() {
        match char {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(char),
            _ => {}
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
        .trim_end_matches(['\n', '\0'])
        .to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_missing_picture_or_sound_decoder_is_essential() {
        let av1 = r#"missing-plugin, type=(string)decoder, detail=(GstCaps)"video/x-av1\,\ interlace-mode\=\(string\)progressive", name=(string)"AV1\ decoder";"#;
        assert!(super::missing_is_essential(av1));
        let opus = r#"missing-plugin, type=(string)decoder, detail=(GstCaps)"audio/x-opus", name=(string)"Opus\ decoder";"#;
        assert!(super::missing_is_essential(opus));
        let bare = r#"missing-plugin, type=(string)decoder, detail=(GstCaps)video/x-av1, name=(string)"AV1\ decoder";"#;
        assert!(super::missing_is_essential(bare));
        let subtitles = r#"missing-plugin, type=(string)decoder, detail=(GstCaps)"application/x-ssa", name=(string)"SSA\ decoder";"#;
        assert!(!super::missing_is_essential(subtitles));
    }

    use super::*;

    #[test]
    fn colorimetry_names_and_numbers() {
        let bt709 = parse_colorimetry("bt709");
        assert_eq!(bt709.range, Some(ColorRange::Limited));
        assert_eq!(bt709.matrix, Some(ColorMatrix::Bt709));
        assert_eq!(bt709.transfer, Some(TransferFunction::Sdr));
        assert_eq!(
            parse_colorimetry("bt2100-pq").transfer,
            Some(TransferFunction::Pq)
        );
        let full601 = parse_colorimetry("1:4:16:2");
        assert_eq!(full601.range, Some(ColorRange::Full));
        assert_eq!(full601.matrix, Some(ColorMatrix::Bt601));
        assert_eq!(
            parse_colorimetry("2:6:15:9").transfer,
            Some(TransferFunction::Hlg)
        );
        assert_eq!(parse_colorimetry("0:0:0:0"), ColorInfo::default());
        assert_eq!(parse_colorimetry("nonsense"), ColorInfo::default());
    }

    #[test]
    fn orientation_tags() {
        assert_eq!(parse_orientation("rotate-90"), Rotation::Cw90);
        assert_eq!(parse_orientation("flip-rotate-270"), Rotation::Cw270);
        assert_eq!(parse_orientation("rotate-0"), Rotation::None);
    }

    #[test]
    fn pixel_aspect_ratio_widens_the_picture() {
        assert_eq!(display_size(720, 576, 16, 15), (768, 576));
        assert_eq!(display_size(1920, 1080, 1, 1), (1920, 1080));
        assert_eq!(display_size(1920, 1080, 0, 1), (1920, 1080));
    }

    #[test]
    fn buffering_ranges_become_times() {
        let duration = Duration::from_secs(100);
        let half = ffi::GST_FORMAT_PERCENT_MAX / 2;
        assert_eq!(
            to_time(ffi::GST_FORMAT_PERCENT, 0, half, duration),
            Some(Duration::ZERO..Duration::from_secs(50))
        );
        assert_eq!(
            to_time(
                ffi::GST_FORMAT_TIME,
                1_000_000_000,
                200_000_000_000,
                duration
            ),
            Some(Duration::from_secs(1)..duration)
        );
        assert_eq!(to_time(ffi::GST_FORMAT_PERCENT, half, half, duration), None);
        assert_eq!(to_time(2, 0, 10, duration), None);
    }

    #[test]
    fn cue_markup_is_removed() {
        assert_eq!(
            strip_markup("<i>Hello</i> &amp; <span foreground=\"red\">bye</span>\n"),
            "Hello & bye"
        );
    }
}
