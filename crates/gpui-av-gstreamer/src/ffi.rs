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
pub const GST_FORMAT_PERCENT: c_int = 5;
/// What 100% is in `GST_FORMAT_PERCENT` values.
pub const GST_FORMAT_PERCENT_MAX: i64 = 1_000_000;
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
/// playbin shows (burns in) the selected text track.
pub const GST_PLAY_FLAG_TEXT: c_uint = 1 << 2;
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

/// The public head of `GstMemory`; only read through a pointer.
#[repr(C)]
pub struct GstMemory {
    pub mini_object: GstMiniObject,
    pub allocator: *mut c_void,
    pub parent: *mut c_void,
    pub maxsize: usize,
    pub align: usize,
    pub offset: usize,
    pub size: usize,
}

#[cfg(all(feature = "karya-gpui", target_os = "linux"))]
/// The public head of `GstVideoMeta`; only read through a pointer.
#[repr(C)]
pub struct GstVideoMeta {
    pub meta_flags: c_uint,
    pub meta_info: *const c_void,
    pub buffer: *mut GstBuffer,
    pub flags: c_uint,
    pub format: c_int,
    pub id: c_int,
    pub width: c_uint,
    pub height: c_uint,
    pub n_planes: c_uint,
    pub offset: [usize; 4],
    pub stride: [c_int; 4],
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
    ($name:ident { $($(#[$attribute:meta])* $function:ident: $signature:ty,)* }) => {
        pub struct $name {
            $($(#[$attribute])* pub $function: $signature,)*
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
    // Only the NV12 path keeps samples alive past the callback.
    #[cfg_attr(not(feature = "karya-gpui"), allow(dead_code))]
    gst_mini_object_ref: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    gst_element_set_state: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    gst_element_get_bus: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    gst_element_get_static_pad: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    gst_element_query_position: unsafe extern "C" fn(*mut c_void, c_int, *mut i64) -> gboolean,
    gst_element_query_duration: unsafe extern "C" fn(*mut c_void, c_int, *mut i64) -> gboolean,
    gst_element_seek_simple: unsafe extern "C" fn(*mut c_void, c_int, c_int, i64) -> gboolean,
    gst_element_query: unsafe extern "C" fn(*mut c_void, *mut c_void) -> gboolean,
    gst_query_new_buffering: unsafe extern "C" fn(c_int) -> *mut c_void,
    gst_query_parse_buffering_range: unsafe extern "C" fn(*mut c_void, *mut c_int, *mut i64, *mut i64, *mut i64),
    gst_query_get_n_buffering_ranges: unsafe extern "C" fn(*mut c_void) -> c_uint,
    gst_query_parse_nth_buffering_range: unsafe extern "C" fn(*mut c_void, c_uint, *mut i64, *mut i64) -> gboolean,
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
    gst_structure_to_string: unsafe extern "C" fn(*const c_void) -> *mut c_char,
    gst_sample_get_buffer: unsafe extern "C" fn(*mut c_void) -> *mut GstBuffer,
    gst_sample_get_caps: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    gst_buffer_map: unsafe extern "C" fn(*mut GstBuffer, *mut GstMapInfo, c_int) -> gboolean,
    gst_buffer_unmap: unsafe extern "C" fn(*mut GstBuffer, *mut GstMapInfo),
    gst_filename_to_uri: unsafe extern "C" fn(*const c_char, *mut *mut GError) -> *mut c_char,
    // Only the dmabuf path reads a buffer's memories and caps features.
    #[cfg_attr(not(all(feature = "karya-gpui", target_os = "linux")), allow(dead_code))]
    gst_buffer_find_memory: unsafe extern "C" fn(*mut GstBuffer, usize, usize, *mut c_uint, *mut c_uint, *mut usize) -> gboolean,
    #[cfg_attr(not(all(feature = "karya-gpui", target_os = "linux")), allow(dead_code))]
    gst_buffer_peek_memory: unsafe extern "C" fn(*mut GstBuffer, c_uint) -> *mut GstMemory,
    #[cfg_attr(not(all(feature = "karya-gpui", target_os = "linux")), allow(dead_code))]
    gst_caps_get_features: unsafe extern "C" fn(*const c_void, c_uint) -> *const c_void,
    #[cfg_attr(not(all(feature = "karya-gpui", target_os = "linux")), allow(dead_code))]
    gst_caps_features_contains: unsafe extern "C" fn(*const c_void, *const c_char) -> gboolean,
});

/// What the dmabuf path needs beyond GStreamer's core: a buffer's plane
/// layout and its memories' file descriptors.
#[cfg(all(feature = "karya-gpui", target_os = "linux"))]
pub struct Dmabuf {
    pub video: GstVideo,
    pub allocators: GstAllocators,
}

#[cfg(all(feature = "karya-gpui", target_os = "linux"))]
library!(GstVideo {
    gst_buffer_get_video_meta: unsafe extern "C" fn(*mut GstBuffer) -> *const GstVideoMeta,
});

#[cfg(all(feature = "karya-gpui", target_os = "linux"))]
library!(GstAllocators {
    gst_is_dmabuf_memory: unsafe extern "C" fn(*mut GstMemory) -> gboolean,
    gst_dmabuf_memory_get_fd: unsafe extern "C" fn(*mut GstMemory) -> c_int,
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
    gst_app_sink_try_pull_sample: unsafe extern "C" fn(*mut c_void, GstClockTime) -> *mut c_void,
});

/// Every library this backend uses, loaded and initialised.
pub struct Api {
    pub glib: Glib,
    pub gobject: GObject,
    pub gst: Gst,
    pub app: GstApp,
    /// Present when GStreamer's allocators library is installed.
    #[cfg(all(feature = "karya-gpui", target_os = "linux"))]
    pub dmabuf: Option<Dmabuf>,
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

/// Where GStreamer lives on macOS when it is not on the loader's own path:
/// Homebrew on Apple silicon, Homebrew on Intel and MacPorts, and the
/// official framework. The loader's default fallback path covers only
/// `/usr/local/lib` of these.
#[cfg(target_os = "macos")]
const DIRECTORIES: [&str; 4] = [
    "/opt/homebrew/lib",
    "/usr/local/lib",
    "/opt/local/lib",
    "/Library/Frameworks/GStreamer.framework/Versions/1.0/lib",
];
#[cfg(not(target_os = "macos"))]
const DIRECTORIES: [&str; 0] = [];

/// Where the official GStreamer installer puts its libraries on Windows:
/// the folder its environment variable names, else the default one. The
/// installer does not add it to `PATH`.
#[cfg(target_os = "windows")]
fn windows_directories() -> Vec<std::path::PathBuf> {
    let mut directories = Vec::new();
    for variable in [
        "GSTREAMER_1_0_ROOT_MSVC_X86_64",
        "GSTREAMER_1_0_ROOT_X86_64",
    ] {
        if let Some(root) = std::env::var_os(variable) {
            directories.push(std::path::Path::new(&root).join("bin"));
        }
    }
    directories.push(std::path::PathBuf::from(
        r"C:\Program Files\gstreamer\1.0\msvc_x86_64\bin",
    ));
    directories
}

/// Loads `name` from the loader's own path, or else from the first of
/// [`DIRECTORIES`] that has it.
fn open(name: &str) -> Result<Library, String> {
    // SAFETY: these are the system's GLib and GStreamer libraries, whose
    // initialisers are safe to run on any thread.
    let first = unsafe { Library::new(name) };
    let error = match first {
        Ok(library) => return Ok(library),
        Err(error) => error,
    };
    for directory in DIRECTORIES {
        let path = std::path::Path::new(directory).join(name);
        if path.exists() {
            // SAFETY: as above.
            return unsafe { Library::new(&path) }
                .map_err(|error| format!("{}: {error}", path.display()));
        }
    }
    #[cfg(target_os = "windows")]
    for directory in windows_directories() {
        let path = directory.join(name);
        if path.exists() {
            // The libraries GStreamer's own need sit beside it, and Windows
            // only looks there for a library loaded by path when told to.
            const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x8;
            // SAFETY: as above.
            return unsafe {
                libloading::os::windows::Library::load_with_flags(
                    &path,
                    LOAD_WITH_ALTERED_SEARCH_PATH,
                )
            }
            .map(Library::from)
            .map_err(|error| format!("{}: {error}", path.display()));
        }
    }
    Err(format!("{name}: {error}"))
}

fn load() -> Result<Api, String> {
    let mut libraries = Vec::with_capacity(NAMES.len());
    for name in NAMES {
        let library =
            open(name).map_err(|error| format!("GStreamer 1.x is not installed ({error})"))?;
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
    #[cfg(all(feature = "karya-gpui", target_os = "linux"))]
    let dmabuf = {
        // SAFETY: GStreamer's allocators library, which the signatures
        // describe; plugins-base installs it with libgstvideo.
        match unsafe { Library::new("libgstallocators-1.0.so.0") } {
            Ok(allocators_library) => {
                // SAFETY: each library is the one the table describes.
                let tables = unsafe {
                    (
                        GstVideo::load(&libraries[4]),
                        GstAllocators::load(&allocators_library),
                    )
                };
                libraries.push(allocators_library);
                match tables {
                    (Ok(video), Ok(allocators)) => Some(Dmabuf { video, allocators }),
                    _ => None,
                }
            }
            Err(_) => None,
        }
    };
    Ok(Api {
        glib,
        gobject,
        gst,
        app,
        #[cfg(all(feature = "karya-gpui", target_os = "linux"))]
        dmabuf,
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
