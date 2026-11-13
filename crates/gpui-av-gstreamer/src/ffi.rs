//! The GStreamer, GObject and GLib functions this backend calls, loaded
//! from the system's shared libraries at run time.
//!
//! Only the handful of functions and struct prefixes the backend needs are
//! declared here, by hand, from the GStreamer 1.x headers. Every struct
//! below is part of GStreamer's stable 1.x ABI.

#![allow(non_camel_case_types, missing_docs)]

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::sync::OnceLock;

use libloading::Library;

pub type gboolean = c_int;
pub type GQuark = u32;
pub type GstClockTime = u64;
pub type GstFlowReturn = c_int;

pub const GST_CLOCK_TIME_NONE: GstClockTime = u64::MAX;
pub const GST_FLOW_OK: GstFlowReturn = 0;
pub const GST_FORMAT_TIME: c_int = 3;
pub const GST_MAP_READ: c_int = 1;

pub const GST_STATE_NULL: c_int = 1;
pub const GST_STATE_PAUSED: c_int = 3;
pub const GST_STATE_PLAYING: c_int = 4;
pub const GST_STATE_CHANGE_FAILURE: c_int = 0;

pub const GST_SEEK_FLAG_FLUSH: c_int = 1 << 0;
pub const GST_SEEK_FLAG_ACCURATE: c_int = 1 << 1;

pub const GST_MESSAGE_EOS: c_int = 1 << 0;
pub const GST_MESSAGE_ERROR: c_int = 1 << 1;
pub const GST_MESSAGE_TAG: c_int = 1 << 4;
pub const GST_MESSAGE_BUFFERING: c_int = 1 << 5;
pub const GST_MESSAGE_STATE_CHANGED: c_int = 1 << 6;
pub const GST_MESSAGE_CLOCK_LOST: c_int = 1 << 10;
pub const GST_MESSAGE_ELEMENT: c_int = 1 << 15;
pub const GST_MESSAGE_DURATION_CHANGED: c_int = 1 << 18;
pub const GST_MESSAGE_ASYNC_DONE: c_int = 1 << 21;
pub const GST_MESSAGE_QOS: c_int = 1 << 24;

/// playbin's `flags`: let the video sink take the decoder's format, since
/// our own sink bin converts.
pub const GST_PLAY_FLAG_NATIVE_VIDEO: c_uint = 1 << 6;

#[repr(C)]
pub struct GError {
    pub domain: GQuark,
    pub code: c_int,
    pub message: *mut c_char,
}

#[repr(C)]
pub struct GstMiniObject {
    pub type_: usize,
    pub refcount: c_int,
    pub lockstate: c_int,
    pub flags: c_uint,
    pub copy: *mut c_void,
    pub dispose: *mut c_void,
    pub free: *mut c_void,
    pub priv_uint: c_uint,
    pub priv_pointer: *mut c_void,
}

/// The public head of `GstMessage`; only read through a pointer.
#[repr(C)]
pub struct GstMessage {
    pub mini_object: GstMiniObject,
    pub type_: c_int,
    pub timestamp: u64,
    pub src: *mut c_void,
    pub seqnum: u32,
}

/// The public head of `GstBuffer`; only read through a pointer.
#[repr(C)]
pub struct GstBuffer {
    pub mini_object: GstMiniObject,
    pub pool: *mut c_void,
    pub pts: GstClockTime,
    pub dts: GstClockTime,
    pub duration: GstClockTime,
    pub offset: u64,
    pub offset_end: u64,
}

#[repr(C)]
pub struct GstMapInfo {
    pub memory: *mut c_void,
    pub flags: c_int,
    pub data: *mut u8,
    pub size: usize,
    pub maxsize: usize,
    pub user_data: [*mut c_void; 4],
    pub reserved: [*mut c_void; 4],
}

impl Default for GstMapInfo {
    fn default() -> Self {
        Self {
            memory: std::ptr::null_mut(),
            flags: 0,
            data: std::ptr::null_mut(),
            size: 0,
            maxsize: 0,
            user_data: [std::ptr::null_mut(); 4],
            reserved: [std::ptr::null_mut(); 4],
        }
    }
}

pub type AppSinkFlowCallback = unsafe extern "C" fn(*mut c_void, *mut c_void) -> GstFlowReturn;

/// `GstAppSinkCallbacks`: three callbacks, then four pointer-sized slots
/// that later 1.x releases filled with optional ones (left empty here).
#[repr(C)]
pub struct GstAppSinkCallbacks {
    pub eos: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    pub new_preroll: Option<AppSinkFlowCallback>,
    pub new_sample: Option<AppSinkFlowCallback>,
    pub reserved: [*mut c_void; 4],
}

pub type GDestroyNotify = unsafe extern "C" fn(*mut c_void);

macro_rules! library {
    ($name:ident { $($function:ident: $signature:ty,)* }) => {
        pub struct $name {
            $(pub $function: $signature,)*
        }

        impl $name {
            /// # Safety
            /// `library` must be the library these signatures describe.
            unsafe fn load(library: &Library) -> Result<Self, String> {
                Ok(Self {
                    $($function: unsafe {
                        *library
                            .get::<$signature>(concat!(stringify!($function), "\0").as_bytes())
                            .map_err(|error| format!("{}: {error}", stringify!($function)))?
                    },)*
                })
            }
        }
    };
}

library!(Glib {
    g_free: unsafe extern "C" fn(*mut c_void),
    g_error_free: unsafe extern "C" fn(*mut GError),
    g_quark_to_string: unsafe extern "C" fn(GQuark) -> *const c_char,
});

library!(GObject {
    g_object_set: unsafe extern "C" fn(*mut c_void, *const c_char, ...),
    g_object_get: unsafe extern "C" fn(*mut c_void, *const c_char, ...),
    g_object_class_find_property: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    g_signal_connect_data: unsafe extern "C" fn(
        *mut c_void,
        *const c_char,
        *mut c_void,
        *mut c_void,
        Option<GDestroyNotify>,
        c_int,
    ) -> std::ffi::c_ulong,
    g_signal_emit_by_name: unsafe extern "C" fn(*mut c_void, *const c_char, ...),
});

library!(Gst {
    gst_init_check: unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char, *mut *mut GError) -> gboolean,
    gst_version: unsafe extern "C" fn(*mut c_uint, *mut c_uint, *mut c_uint, *mut c_uint),
    gst_element_factory_make: unsafe extern "C" fn(*const c_char, *const c_char) -> *mut c_void,
    gst_parse_bin_from_description: unsafe extern "C" fn(*const c_char, gboolean, *mut *mut GError) -> *mut c_void,
    gst_bin_get_by_name: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    gst_object_unref: unsafe extern "C" fn(*mut c_void),
    gst_object_ref_sink: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    gst_mini_object_unref: unsafe extern "C" fn(*mut c_void),
    gst_element_set_state: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    gst_element_get_bus: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    gst_element_get_static_pad: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    gst_element_query_position: unsafe extern "C" fn(*mut c_void, c_int, *mut i64) -> gboolean,
    gst_element_query_duration: unsafe extern "C" fn(*mut c_void, c_int, *mut i64) -> gboolean,
    gst_element_seek_simple: unsafe extern "C" fn(*mut c_void, c_int, c_int, i64) -> gboolean,
    gst_bus_pop: unsafe extern "C" fn(*mut c_void) -> *mut GstMessage,
    gst_message_parse_error: unsafe extern "C" fn(*mut GstMessage, *mut *mut GError, *mut *mut c_char),
    gst_message_parse_state_changed: unsafe extern "C" fn(*mut GstMessage, *mut c_int, *mut c_int, *mut c_int),
    gst_message_parse_buffering: unsafe extern "C" fn(*mut GstMessage, *mut c_int),
    gst_message_parse_tag: unsafe extern "C" fn(*mut GstMessage, *mut *mut c_void),
    gst_message_parse_qos_stats: unsafe extern "C" fn(*mut GstMessage, *mut c_int, *mut u64, *mut u64),
    gst_message_get_structure: unsafe extern "C" fn(*mut GstMessage) -> *const c_void,
    gst_tag_list_get_string: unsafe extern "C" fn(*const c_void, *const c_char, *mut *mut c_char) -> gboolean,
    gst_pad_get_current_caps: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    gst_caps_get_structure: unsafe extern "C" fn(*const c_void, c_uint) -> *const c_void,
    gst_structure_get_name: unsafe extern "C" fn(*const c_void) -> *const c_char,
    gst_structure_get_int: unsafe extern "C" fn(*const c_void, *const c_char, *mut c_int) -> gboolean,
    gst_structure_get_fraction: unsafe extern "C" fn(*const c_void, *const c_char, *mut c_int, *mut c_int) -> gboolean,
    gst_structure_get_string: unsafe extern "C" fn(*const c_void, *const c_char) -> *const c_char,
    gst_structure_new_empty: unsafe extern "C" fn(*const c_char) -> *mut c_void,
    gst_structure_set: unsafe extern "C" fn(*mut c_void, *const c_char, ...),
    gst_structure_free: unsafe extern "C" fn(*mut c_void),
    gst_sample_get_buffer: unsafe extern "C" fn(*mut c_void) -> *mut GstBuffer,
    gst_sample_get_caps: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    gst_buffer_map: unsafe extern "C" fn(*mut GstBuffer, *mut GstMapInfo, c_int) -> gboolean,
    gst_buffer_unmap: unsafe extern "C" fn(*mut GstBuffer, *mut GstMapInfo),
    gst_filename_to_uri: unsafe extern "C" fn(*const c_char, *mut *mut GError) -> *mut c_char,
});

library!(GstApp {
    gst_app_sink_set_callbacks: unsafe extern "C" fn(
        *mut c_void,
        *const GstAppSinkCallbacks,
        *mut c_void,
        Option<GDestroyNotify>,
    ),
    gst_app_sink_pull_sample: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    gst_app_sink_pull_preroll: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
});

/// Every library this backend uses, loaded and initialised.
pub struct Api {
    pub glib: Glib,
    pub gobject: GObject,
    pub gst: Gst,
    pub app: GstApp,
    /// GStreamer's version, for logs and the experiment's notes.
    pub version: (u32, u32, u32),
    // Unloading GStreamer while its threads run would crash; the libraries
    // stay loaded for the life of the process.
    _libraries: Vec<Library>,
}

// SAFETY: the table holds function pointers into thread-safe C libraries
// and the libraries themselves, which are never unloaded.
unsafe impl Send for Api {}
// SAFETY: as above; nothing in the table changes after loading.
unsafe impl Sync for Api {}

#[cfg(target_os = "linux")]
const NAMES: [&str; 5] = [
    "libglib-2.0.so.0",
    "libgobject-2.0.so.0",
    "libgstreamer-1.0.so.0",
    "libgstapp-1.0.so.0",
    // Unused symbols, but its presence means plugins-base is installed.
    "libgstvideo-1.0.so.0",
];
#[cfg(target_os = "macos")]
const NAMES: [&str; 5] = [
    "libglib-2.0.0.dylib",
    "libgobject-2.0.0.dylib",
    "libgstreamer-1.0.0.dylib",
    "libgstapp-1.0.0.dylib",
    "libgstvideo-1.0.0.dylib",
];
#[cfg(target_os = "windows")]
const NAMES: [&str; 5] = [
    "glib-2.0-0.dll",
    "gobject-2.0-0.dll",
    "gstreamer-1.0-0.dll",
    "gstapp-1.0-0.dll",
    "gstvideo-1.0-0.dll",
];
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
const NAMES: [&str; 5] = [
    "libglib-2.0.so",
    "libgobject-2.0.so",
    "libgstreamer-1.0.so",
    "libgstapp-1.0.so",
    "libgstvideo-1.0.so",
];

/// Loads and initialises GStreamer once per process. The error says what
/// was missing, for a person.
pub fn api() -> Result<&'static Api, &'static str> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(load).as_ref().map_err(String::as_str)
}

fn load() -> Result<Api, String> {
    let mut libraries = Vec::with_capacity(NAMES.len());
    for name in NAMES {
        // SAFETY: these are the system's GLib and GStreamer libraries, whose
        // initialisers are safe to run on any thread.
        let library = unsafe { Library::new(name) }
            .map_err(|error| format!("GStreamer 1.x is not installed ({name}: {error})"))?;
        libraries.push(library);
    }
    // SAFETY: each library is the one the table's signatures describe.
    let (glib, gobject, gst, app) = unsafe {
        (
            Glib::load(&libraries[0])?,
            GObject::load(&libraries[1])?,
            Gst::load(&libraries[2])?,
            GstApp::load(&libraries[3])?,
        )
    };
    let mut error = std::ptr::null_mut();
    // SAFETY: gst_init_check accepts null arguments and is safe to call
    // more than once; a failure fills `error`.
    if unsafe { (gst.gst_init_check)(std::ptr::null_mut(), std::ptr::null_mut(), &mut error) } == 0
    {
        // SAFETY: on failure `error` is a GError we own, or null.
        let message = unsafe { take_error(&glib, error) }
            .map(|(_, _, message)| message)
            .unwrap_or_default();
        return Err(format!("GStreamer did not start: {message}"));
    }
    let (mut major, mut minor, mut micro, mut nano) = (0, 0, 0, 0);
    // SAFETY: plain out parameters.
    unsafe { (gst.gst_version)(&mut major, &mut minor, &mut micro, &mut nano) };
    Ok(Api {
        glib,
        gobject,
        gst,
        app,
        version: (major, minor, micro),
        _libraries: libraries,
    })
}

/// Reads and frees a GError: its domain's name, code and message.
///
/// # Safety
/// `error` must be null or a GError the caller owns.
pub unsafe fn take_error(glib: &Glib, error: *mut GError) -> Option<(String, c_int, String)> {
    if error.is_null() {
        return None;
    }
    // SAFETY: a valid GError, per the contract.
    unsafe {
        let domain = string((glib.g_quark_to_string)((*error).domain)).unwrap_or_default();
        let code = (*error).code;
        let message = string((*error).message).unwrap_or_default();
        (glib.g_error_free)(error);
        Some((domain, code, message))
    }
}

/// Copies a C string, if there is one.
///
/// # Safety
/// `pointer` must be null or a NUL-terminated string.
pub unsafe fn string(pointer: *const c_char) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    // SAFETY: per the contract.
    Some(
        unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned(),
    )
}
