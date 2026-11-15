//! One media, played by one `<video>` element.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gpui::{App, Pixels, Point, Size, Window, point, size};
use gpui_av::{Error, ErrorKind, EventSink, MediaInfo, Placement, PlaybackState, Session, Stats};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::{JsFuture, spawn_local};

use super::probe::probe;
use super::proxy::Proxy;
use crate::errors;
use crate::layout::{Canvas, Layout};

/// The element's style apart from where it is. It sits above the canvas,
/// lets every pointer event through to GPUI, and stretches the picture to
/// its box: gpui-av has applied the object fit to the box already.
const BASE_CSS: &str = "position:fixed;margin:0;padding:0;border:0;\
                        pointer-events:none;object-fit:fill;z-index:1;";
/// `HTMLMediaElement.HAVE_METADATA`.
const HAVE_METADATA: u16 = 1;
/// How much of the stated length may be missing at `ended` before the end
/// counts as the file stopping early.
const EARLY_END_SECONDS: f64 = 1.0;

/// The page's monotonic clock, in milliseconds.
fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map_or(0.0, |performance| performance.now())
}

type Listener = Closure<dyn FnMut(web_sys::Event)>;
/// What runs when the element fires an event.
type Handler = fn(&Rc<Shared>);

/// The page's events after which the canvas may be somewhere else.
const PAGE_EVENTS: [&str; 2] = ["scroll", "resize"];

pub(crate) struct WebSession {
    shared: Rc<Shared>,
    listeners: Vec<(&'static str, Listener)>,
    /// Listens to [`PAGE_EVENTS`] on the page's window.
    page_listener: Listener,
    /// The service worker registration of a source with headers.
    proxied: Option<(Rc<Proxy>, String)>,
}

/// What the session, its event listeners and its overlay guard share.
struct Shared {
    video: web_sys::HtmlVideoElement,
    events: EventSink,
    /// The URL the element loads: what a probe asks for after an error.
    url: String,
    /// The app asked for playback and has not paused since.
    wanted: Cell<bool>,
    /// The element could start playing: `Ready` has been reported, or
    /// `Playing` is about to be.
    ready: Cell<bool>,
    /// `Ended` was the last state reported.
    ended: Cell<bool>,
    failed: Cell<bool>,
    /// The session is gone; late tasks must do nothing.
    closed: Cell<bool>,
    /// Seeks the app asked for that have not finished. The browser seeks on
    /// its own too (a loop, a restart after the end).
    seeks: Cell<u32>,
    /// Where playback last was, and when (`performance.now()`), while it
    /// moves on its own: to tell a real end from a file that stops early.
    progress: Cell<Option<(f64, f64)>>,
    /// The properties last reported.
    info: RefCell<Option<MediaInfo>>,
    /// The placement last applied; empty while the element is hidden.
    css: RefCell<String>,
    /// What the element was last painted with, while it shows.
    painted: RefCell<Option<Painted>>,
}

/// What `paint` was last given. GPUI only paints when its own window
/// changes, so this is kept to move the overlay when the page moves the
/// canvas instead: a scroll, or a layout change around it.
struct Painted {
    placement: Placement,
    canvas: Option<web_sys::HtmlCanvasElement>,
    /// The window's size in GPUI's logical pixels.
    viewport: Size<Pixels>,
}

impl Painted {
    /// The element's CSS for where the canvas is now; empty when nothing
    /// of the picture shows.
    fn css(&self) -> String {
        let canvas = measure(self.canvas.as_ref(), self.viewport);
        Layout::new(&self.placement, canvas)
            .map(|layout| layout.css())
            .unwrap_or_default()
    }
}

impl WebSession {
    pub fn open(
        url: &str,
        headers: &[(String, String)],
        proxy: Option<Rc<Proxy>>,
        events: EventSink,
    ) -> Result<Self, JsValue> {
        let window = web_sys::window().ok_or("no window")?;
        let document = window.document().ok_or("no document")?;
        let page = window.location().href()?;
        let url = web_sys::Url::new_with_base(url, &page)?;

        let video: web_sys::HtmlVideoElement = document.create_element("video")?.dyn_into()?;
        // Without this, iOS plays in its own full-screen player.
        video.set_attribute("playsinline", "")?;
        video.set_preload("auto");
        video
            .style()
            .set_css_text(&format!("{BASE_CSS}display:none;"));
        document.body().ok_or("no body")?.append_child(&video)?;

        let proxied = proxy.map(|proxy| {
            let (id, made_up) = proxy.add(&url.href(), headers);
            (proxy, id, made_up)
        });
        let load = match &proxied {
            Some((_, _, made_up)) => made_up.clone(),
            None => url.href(),
        };
        let shared = Rc::new(Shared {
            video,
            events,
            url: load.clone(),
            wanted: Cell::new(false),
            ready: Cell::new(false),
            ended: Cell::new(false),
            failed: Cell::new(false),
            closed: Cell::new(false),
            seeks: Cell::new(0),
            progress: Cell::new(None),
            info: RefCell::new(None),
            css: RefCell::new(String::new()),
            painted: RefCell::new(None),
        });
        let listeners = Shared::listen(&shared)?;
        let page_listener = Listener::new({
            let shared = shared.clone();
            move |_| shared.follow_canvas()
        });
        for name in PAGE_EVENTS {
            // Capturing, so that a scroll inside any element counts.
            window.add_event_listener_with_callback_and_bool(
                name,
                page_listener.as_ref().unchecked_ref(),
                true,
            )?;
        }

        match &proxied {
            Some((proxy, ..)) => {
                // The made-up URL only answers once the worker is running.
                let (proxy, shared) = (proxy.clone(), shared.clone());
                spawn_local(async move {
                    let ready = proxy.ready().await;
                    if shared.closed.get() {
                        return;
                    }
                    match ready {
                        Ok(()) => shared.video.set_src(&load),
                        Err(error) => shared.fail(error),
                    }
                });
            }
            None => {
                // A cross-origin isolated page may only load another
                // origin's media with CORS (or the server's explicit
                // Cross-Origin-Resource-Policy); ask for CORS.
                let isolated = js_sys::Reflect::get(&window, &"crossOriginIsolated".into())
                    .is_ok_and(|isolated| isolated.is_truthy());
                if isolated && url.origin() != window.location().origin()? {
                    shared.video.set_cross_origin(Some("anonymous"));
                }
                shared.video.set_src(&load);
            }
        }
        Ok(Self {
            shared,
            listeners,
            page_listener,
            proxied: proxied.map(|(proxy, id, _)| (proxy, id)),
        })
    }
}

impl Shared {
    /// Listens to the element's events for as long as the session lives.
    fn listen(shared: &Rc<Self>) -> Result<Vec<(&'static str, Listener)>, JsValue> {
        let handlers: [(&'static str, Handler); 12] = [
            ("loadedmetadata", |shared| shared.refresh_info()),
            ("durationchange", |shared| shared.refresh_info()),
            ("resize", |shared| shared.refresh_info()),
            ("loadeddata", |shared| shared.refresh_info()),
            ("timeupdate", |shared| {
                shared.refresh_info();
                shared.note_progress(false);
            }),
            ("canplay", |shared| shared.on_can_play()),
            ("playing", |shared| shared.on_playing()),
            ("pause", |shared| shared.on_pause()),
            ("waiting", |shared| shared.on_waiting()),
            ("ended", |shared| shared.on_ended()),
            ("seeked", |shared| shared.on_seeked()),
            ("error", Shared::on_error),
        ];
        handlers
            .into_iter()
            .map(|(name, handler)| {
                let listener = Listener::new({
                    let shared = shared.clone();
                    move |_| {
                        if !shared.closed.get() && !shared.failed.get() {
                            handler(&shared);
                        }
                    }
                });
                shared
                    .video
                    .add_event_listener_with_callback(name, listener.as_ref().unchecked_ref())?;
                Ok((name, listener))
            })
            .collect()
    }

    fn state(&self, state: PlaybackState) {
        self.ended.set(state == PlaybackState::Ended);
        self.events.state(state);
    }

    fn fail(&self, error: Error) {
        self.failed.set(true);
        self.events.fail(error);
    }

    /// What the element knows about the media now.
    fn read_info(&self) -> MediaInfo {
        let duration = Some(self.video.duration())
            .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
            .map(Duration::from_secs_f64);
        // The size as shown: the browser has applied the rotation tag and
        // the pixel aspect ratio, and does not say what they were.
        let (width, height) = (self.video.video_width(), self.video.video_height());
        let natural_size = (width > 0 && height > 0).then(|| size(width, height));
        let has_video = natural_size.is_some();
        MediaInfo {
            duration,
            natural_size,
            has_video,
            // Media without a picture that still loaded is audio.
            has_audio: self.has_audio().or((!has_video).then_some(true)),
            ..MediaInfo::default()
        }
    }

    /// Whether there is an audio track, where the browser lets on: the
    /// track list (Safari), Firefox's own flag, or Chrome's count of audio
    /// bytes decoded so far.
    fn has_audio(&self) -> Option<bool> {
        let get = |name: &str| {
            js_sys::Reflect::get(&self.video, &name.into())
                .ok()
                .filter(|value| !value.is_undefined())
        };
        if let Some(tracks) = get("audioTracks")
            && let Some(count) = js_sys::Reflect::get(&tracks, &"length".into())
                .ok()
                .and_then(|count| count.as_f64())
        {
            return Some(count > 0.0);
        }
        if let Some(has_audio) = get("mozHasAudio").and_then(|value| value.as_bool()) {
            return Some(has_audio);
        }
        let audio_bytes = get("webkitAudioDecodedByteCount")?.as_f64()?;
        if audio_bytes > 0.0 {
            return Some(true);
        }
        // Video has been decoded and played for a while, and no audio.
        let video_bytes = get("webkitVideoDecodedByteCount")?.as_f64()?;
        (video_bytes > 0.0 && self.video.current_time() > 0.5).then_some(false)
    }

    /// Reports the media's properties when they are first known and
    /// whenever they change.
    fn refresh_info(&self) {
        if self.video.ready_state() < HAVE_METADATA {
            return;
        }
        let info = self.read_info();
        if self.info.borrow().as_ref() != Some(&info) {
            *self.info.borrow_mut() = Some(info.clone());
            self.events.info(info);
        }
    }

    fn on_can_play(&self) {
        self.refresh_info();
        // When playback was asked for already, `playing` reports the state.
        if !self.ready.replace(true) && !self.wanted.get() {
            self.state(PlaybackState::Ready);
        }
    }

    fn on_playing(&self) {
        self.refresh_info();
        self.note_progress(true);
        self.ready.set(true);
        self.state(PlaybackState::Playing);
    }

    fn on_pause(&self) {
        // The browser pauses the element at the end; `ended` reports that.
        if self.ready.get() && !self.video.ended() {
            self.state(PlaybackState::Paused);
        }
    }

    fn on_waiting(&self) {
        if self.wanted.get() && !self.video.paused() {
            self.state(PlaybackState::Buffering);
        }
    }

    /// Notes where playback is now, while it plays at normal speed. Unless
    /// `reset` (a start or a seek), a jump further ahead than playing could
    /// have got is not progress: Chrome jumps to the stated end of a file
    /// cut short, with a `timeupdate`, just before `ended`.
    fn note_progress(&self, reset: bool) {
        let playing = !self.video.paused() && self.video.playback_rate() == 1.0;
        if !playing {
            // Chrome pauses the element just before that jump; keep the
            // last progress for `ended` to judge. A start or a seek resets.
            if reset {
                self.progress.set(None);
            }
            return;
        }
        let (position, now) = (self.video.current_time(), now_ms());
        if !reset
            && let Some((last, at)) = self.progress.get()
            && position - (last + (now - at) / 1000.0) > EARLY_END_SECONDS
        {
            return;
        }
        self.progress.set(Some((position, now)));
    }

    fn on_ended(&self) {
        self.wanted.set(false);
        // A file cut off inside its media data: Chrome plays to the cut,
        // then jumps to the stated end and fires `ended`, with no error.
        // Playing at normal speed from the last progress, the end could not
        // have been reached yet.
        let duration = self.video.duration();
        if let Some((position, at)) = self.progress.take()
            && duration.is_finite()
        {
            let reachable = position + (now_ms() - at) / 1000.0;
            if duration - reachable > EARLY_END_SECONDS {
                self.fail(Error::new(
                    ErrorKind::Decode,
                    format!(
                        "the media stops after {position:.1} s of its stated {duration:.1} s: \
                         the file is cut short or damaged"
                    ),
                ));
                return;
            }
        }
        self.state(PlaybackState::Ended);
    }

    fn on_seeked(&self) {
        self.note_progress(true);
        if self.seeks.replace(0) == 0 {
            return;
        }
        self.events.seeked(self.position());
        // A seek away from the end leaves the media paused there.
        if self.ended.get() && self.video.paused() && !self.video.ended() {
            self.state(PlaybackState::Paused);
        }
    }

    fn on_error(self: &Rc<Self>) {
        // No more events after this one; the probe below takes a moment.
        self.failed.set(true);
        let (code, detail) = self
            .video
            .error()
            .map_or((0, String::new()), |error| (error.code(), error.message()));
        let shared = self.clone();
        spawn_local(async move {
            let probe = probe(&shared.url).await;
            if !shared.closed.get() {
                shared.events.fail(Error::new(
                    errors::kind(code, probe),
                    errors::message(code, probe, &detail),
                ));
            }
        });
    }

    fn position(&self) -> Duration {
        Some(self.video.current_time())
            .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
            .map_or(Duration::ZERO, Duration::from_secs_f64)
    }

    /// Moves the element to `css` (a [`Layout`]'s), or hides it when empty.
    /// The style is only touched when it changes.
    fn place(&self, css: String) {
        if *self.css.borrow() == css {
            return;
        }
        let placed = if css.is_empty() {
            "display:none;"
        } else {
            &css
        };
        self.video
            .style()
            .set_css_text(&format!("{BASE_CSS}{placed}"));
        *self.css.borrow_mut() = css;
    }

    /// Puts the element where the canvas is now, after the page moved it
    /// without GPUI painting.
    fn follow_canvas(&self) {
        let css = match &*self.painted.borrow() {
            Some(painted) => painted.css(),
            None => return,
        };
        self.place(css);
    }
}

/// Held by the `video()` element while it shows the picture; hides the
/// overlay when it stops.
struct Overlay(Rc<Shared>);

impl Drop for Overlay {
    fn drop(&mut self) {
        self.0.painted.replace(None);
        self.0.place(String::new());
    }
}

impl Session for WebSession {
    fn play(&mut self) {
        let shared = self.shared.clone();
        shared.wanted.set(true);
        let Ok(started) = shared.video.play() else {
            return;
        };
        spawn_local(async move {
            let Err(error) = JsFuture::from(started).await else {
                return;
            };
            // A browser refuses to start audible playback that no click or
            // key press led to. Other refusals arrive as `error` or follow
            // a pause the app asked for.
            let refused = error
                .dyn_ref::<web_sys::DomException>()
                .is_some_and(|error| error.name() == "NotAllowedError");
            if !refused || shared.closed.get() || shared.failed.get() {
                return;
            }
            web_sys::console::warn_1(
                &"gpui-av: the browser refused to start playback without a user gesture; \
                  play from a click or key press, or muted"
                    .into(),
            );
            shared.wanted.set(false);
            if shared.ready.get() {
                shared.state(PlaybackState::Paused);
            } else if shared.video.ready_state() > HAVE_METADATA {
                shared.on_can_play();
            }
        });
    }

    fn pause(&mut self) {
        self.shared.wanted.set(false);
        let _ = self.shared.video.pause();
    }

    fn seek(&mut self, position: Duration) {
        self.shared.seeks.set(self.shared.seeks.get() + 1);
        self.shared.video.set_current_time(position.as_secs_f64());
    }

    fn set_volume(&mut self, volume: f32) {
        self.shared
            .video
            .set_volume(f64::from(volume.clamp(0.0, 1.0)));
    }

    fn set_muted(&mut self, muted: bool) {
        self.shared.video.set_muted(muted);
    }

    fn set_looping(&mut self, looping: bool) {
        self.shared.video.set_loop(looping);
    }

    fn position(&self) -> Duration {
        self.shared.position()
    }

    fn buffered(&self) -> Vec<std::ops::Range<Duration>> {
        let ranges = self.shared.video.buffered();
        (0..ranges.length())
            .filter_map(|index| {
                let start = ranges.start(index).ok()?;
                let end = ranges.end(index).ok()?;
                let start = Duration::try_from_secs_f64(start).ok()?;
                let end = Duration::try_from_secs_f64(end).ok()?;
                (start < end).then_some(start..end)
            })
            .collect()
    }

    fn stats(&self) -> Stats {
        // The browser's own counters. Drift is the browser's to keep and it
        // does not report it.
        let quality = self.shared.video.get_video_playback_quality();
        let dropped = quality.dropped_video_frames();
        let presented = quality.total_video_frames().saturating_sub(dropped);
        Stats::new(u64::from(presented), u64::from(dropped))
    }

    fn mount(&mut self) -> Option<Box<dyn Any>> {
        Some(Box::new(Overlay(self.shared.clone())))
    }

    // The browser shows the frames itself; GPUI only says where.
    fn needs_frames(&self) -> bool {
        false
    }

    fn paint(&mut self, placement: Placement, window: &mut Window, _cx: &mut App) {
        let painted = Painted {
            placement,
            canvas: canvas(window),
            viewport: window.viewport_size(),
        };
        self.shared.place(painted.css());
        self.shared.painted.replace(Some(painted));
    }
}

impl Drop for WebSession {
    fn drop(&mut self) {
        let video = &self.shared.video;
        self.shared.closed.set(true);
        for (name, listener) in &self.listeners {
            let _ =
                video.remove_event_listener_with_callback(name, listener.as_ref().unchecked_ref());
        }
        // Stop the download and free the decoder before the element goes.
        let _ = video.pause();
        let _ = video.remove_attribute("src");
        video.load();
        video.remove();
        if let Some(window) = web_sys::window() {
            for name in PAGE_EVENTS {
                let _ = window.remove_event_listener_with_callback_and_bool(
                    name,
                    self.page_listener.as_ref().unchecked_ref(),
                    true,
                );
            }
        }
        if let Some((proxy, id)) = &self.proxied {
            proxy.remove(id);
        }
    }
}

/// Where a window's canvas is in the browser's viewport, measured from the
/// canvas itself; `viewport` is the window's size in GPUI's logical pixels.
fn measure(canvas: Option<&web_sys::HtmlCanvasElement>, viewport: Size<Pixels>) -> Canvas {
    let unscaled = Canvas {
        origin: Point::default(),
        scale: point(1.0, 1.0),
    };
    let Some(canvas) = canvas else {
        return unscaled;
    };
    let rect = canvas.get_bounding_client_rect();
    let (width, height) = (f32::from(viewport.width), f32::from(viewport.height));
    if width <= 0.0 || height <= 0.0 {
        return unscaled;
    }
    Canvas {
        origin: point(rect.left() as f32, rect.top() as f32),
        scale: point(rect.width() as f32 / width, rect.height() as f32 / height),
    }
}

/// The canvas a GPUI window draws into.
fn canvas(window: &Window) -> Option<web_sys::HtmlCanvasElement> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::WebCanvas(handle) = handle.as_raw() else {
        return None;
    };
    // SAFETY: raw-window-handle documents `WebCanvasWindowHandle::obj` as a
    // pointer to the `JsValue` of an `HtmlCanvasElement`, valid for as long
    // as the handle borrowed from the window is, which covers this call.
    let value: &JsValue = unsafe { handle.obj.cast::<JsValue>().as_ref() };
    value.clone().dyn_into().ok()
}
