//! One piece of media playing through `AVPlayer`.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use block2::RcBlock;
use gpui::{App, SharedString, Task, Window};
use gpui_av::{EventSink, Placement, PlaybackState, Rotation, Session, Stats, TrackKind};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{AnyThread, MainThreadMarker};
use objc2_av_foundation::{
    AVPlayer, AVPlayerActionAtItemEnd, AVPlayerItem, AVPlayerItemDidPlayToEndTimeNotification,
    AVPlayerItemFailedToPlayToEndTimeErrorKey, AVPlayerItemFailedToPlayToEndTimeNotification,
    AVPlayerItemStatus, AVPlayerItemVideoOutput, AVPlayerTimeControlStatus, AVSampleCursor,
    AVURLAsset, AVVideoColorPrimaries_ITU_R_709_2, AVVideoColorPrimariesKey,
    AVVideoColorPropertiesKey, AVVideoComposition, AVVideoTransferFunction_ITU_R_709_2,
    AVVideoTransferFunctionKey, AVVideoYCbCrMatrix_ITU_R_601_4, AVVideoYCbCrMatrixKey,
    NSValueAVFoundationExtensions,
};
use objc2_core_foundation::{CFRetained, CFString};
use objc2_core_media::{CMTime, kCMTimeInvalid, kCMTimeZero};
use objc2_core_video::{
    CVGetCurrentHostTime, CVPixelBuffer, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferMetalCompatibilityKey, kCVPixelBufferPixelFormatTypeKey,
    kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
    kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange,
};
use objc2_foundation::{
    NSDictionary, NSError, NSNotification, NSNotificationCenter, NSNumber, NSObjectProtocol,
    NSOperationQueue, NSString,
};

use super::errors;
use super::info::{self, Loaded};
use super::loader::Loader;
use super::probe::{self, HttpSource};
use super::tracks::{Groups, Legible};

/// How often the session checks the player for state changes. AVFoundation
/// reports most of them through key-value observing, which needs an
/// Objective-C class; polling a few properties is cheaper to maintain.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// How long the session keeps asking for window frames while it waits for
/// a picture that has not arrived (the first frame, or the frame after a
/// seek while paused).
const FRAME_WAIT: Duration = Duration::from_secs(3);

/// How long after a seek, a loop or a resume drift is not measured.
const SETTLE: Duration = Duration::from_millis(250);

/// Two sample times this close are the same frame.
const SAME_FRAME: Duration = Duration::from_millis(1);

pub(crate) struct AvSession {
    inner: Rc<RefCell<Inner>>,
    /// Our own HTTP loading, for a source with headers: AVFoundation holds
    /// its delegate weakly, so the session keeps it. Declared after `inner`
    /// so the player goes first.
    _loader: Option<Loader>,
    observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
    _poll: Task<()>,
}

/// What the notification blocks leave for the next poll.
#[derive(Default)]
struct Notes {
    ended: Cell<bool>,
    failed: RefCell<Option<gpui_av::Error>>,
}

/// The seek in flight, shared with the completion block, which AVFoundation
/// may call on any thread.
#[derive(Default)]
struct SeekState {
    generation: AtomicU64,
    in_flight: AtomicBool,
}

struct Inner {
    player: Retained<AVPlayer>,
    item: Retained<AVPlayerItem>,
    output: Retained<AVPlayerItemVideoOutput>,
    events: EventSink,
    /// The URL and headers, for an HTTP source.
    http: Option<HttpSource>,
    notes: Rc<Notes>,
    seek: Arc<SeekState>,
    /// The last state sent, so a state is reported once.
    reported: Option<PlaybackState>,
    loaded: Option<Loaded>,
    failed: bool,
    /// The app asked for playback (and has not paused since).
    started: bool,
    ended: bool,
    looping: bool,
    /// A seek asked for before the media loaded.
    pending_seek: Option<Duration>,
    /// The frame on screen.
    frame: Option<CFRetained<CVPixelBuffer>>,
    /// The presentation time of the frame on screen.
    frame_time: Option<Duration>,
    /// A cursor on the sample of the frame on screen, for counting the
    /// samples skipped before the next one.
    cursor: Option<Retained<AVSampleCursor>>,
    /// Drift is measured from this moment on: a seek, a loop or a resume
    /// puts the picture and the clock briefly apart on purpose.
    drift_from: Option<Instant>,
    max_drift: Option<Duration>,
    /// Waiting for a new picture since then.
    awaiting_frame: Option<Instant>,
    stats: Stats,
    /// The media's audio and text groups, once loaded.
    groups: Option<Groups>,
    /// Track choices made before the media loaded.
    pending_tracks: Vec<(TrackKind, Option<usize>)>,
    /// Subtitle text inside the media.
    legible: Legible,
    /// The local server a Matroska or WebM source plays from. Last, so the
    /// player and item go first.
    remuxed: Option<gpui_av_remux::HlsServer>,
}

impl AvSession {
    pub(crate) fn new(
        asset: Retained<AVURLAsset>,
        loader: Option<Loader>,
        remuxed: Option<gpui_av_remux::HlsServer>,
        http: Option<HttpSource>,
        events: EventSink,
        mtm: MainThreadMarker,
        cx: &mut App,
    ) -> Self {
        // SAFETY: creating AVFoundation objects on the main thread, with
        // arguments of the documented types.
        let (player, item, output) = unsafe {
            let item = AVPlayerItem::playerItemWithAsset(&asset, mtm);
            let output = AVPlayerItemVideoOutput::initWithOutputSettings(
                AVPlayerItemVideoOutput::alloc(),
                Some(&output_settings()),
            );
            item.addOutput(&output);
            let player = AVPlayer::playerWithPlayerItem(Some(&item), mtm);
            player.setActionAtItemEnd(AVPlayerActionAtItemEnd::Pause);
            if loader.is_some() {
                // Apple's advice for a delegate that loads the media: the
                // stall prediction assumes AVFoundation's own loading.
                // Not for the remuxer's HLS: without waiting, `play()`
                // before the first segment arrives stalls the player for
                // good (it reports playing and the clock stays at zero).
                player.setAutomaticallyWaitsToMinimizeStalling(false);
            }
            (player, item, output)
        };
        let legible = Legible::attach(&item);
        let notes = Rc::new(Notes::default());
        let inner = Rc::new(RefCell::new(Inner {
            player,
            item,
            output,
            events,
            http,
            notes: notes.clone(),
            seek: Arc::default(),
            reported: Some(PlaybackState::Loading),
            loaded: None,
            failed: false,
            started: false,
            ended: false,
            looping: false,
            pending_seek: None,
            frame: None,
            frame_time: None,
            cursor: None,
            drift_from: Some(Instant::now() + SETTLE),
            max_drift: None,
            awaiting_frame: Some(Instant::now()),
            stats: Stats::default(),
            groups: None,
            pending_tracks: Vec::new(),
            legible,
            remuxed,
        }));
        let observers = observe(&inner, &notes);
        let weak = Rc::downgrade(&inner);
        let poll = cx.spawn(async move |cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let Some(inner) = weak.upgrade() else { break };
                let Ok(mut inner) = inner.try_borrow_mut() else {
                    continue;
                };
                inner.poll();
                if inner.failed {
                    break;
                }
            }
        });
        Self {
            inner,
            observers,
            _poll: poll,
            _loader: loader,
        }
    }
}

/// The settings for the frames GPUI draws. Stock GPUI's Metal surface takes
/// only full-range NV12 (`420f`), backed by an IOSurface, and converts it to
/// RGB with the BT.601 matrix, so the output asks AVFoundation to convert
/// every frame to exactly that. Karya Labs' GPUI (feature `karya-gpui`,
/// spec 4.3) takes 10-bit frames with their own colour tags instead, so
/// frames keep their depth, matrix and HDR transfer.
fn output_settings() -> Retained<NSDictionary<NSString, AnyObject>> {
    if cfg!(feature = "karya-gpui") {
        return native_output_settings();
    }
    // SAFETY: CoreVideo's keys are CFStrings, toll-free bridged to
    // NSString; AVFoundation's keys are constant NSStrings.
    unsafe {
        let key = |key: &CFString| -> &NSString { &*(key as *const CFString).cast::<NSString>() };
        let colour_keys = [
            AVVideoColorPrimariesKey.unwrap(),
            AVVideoTransferFunctionKey.unwrap(),
            AVVideoYCbCrMatrixKey.unwrap(),
        ];
        let colour_values: [&AnyObject; 3] = [
            AVVideoColorPrimaries_ITU_R_709_2.unwrap(),
            AVVideoTransferFunction_ITU_R_709_2.unwrap(),
            AVVideoYCbCrMatrix_ITU_R_601_4.unwrap(),
        ];
        let colour = NSDictionary::from_slices(&colour_keys, &colour_values);
        let format = NSNumber::new_u32(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange);
        let io_surface = NSDictionary::<NSString, AnyObject>::new();
        let metal = NSNumber::new_bool(true);
        let keys = [
            key(kCVPixelBufferPixelFormatTypeKey),
            key(kCVPixelBufferIOSurfacePropertiesKey),
            key(kCVPixelBufferMetalCompatibilityKey),
            AVVideoColorPropertiesKey.unwrap(),
        ];
        let values: [&AnyObject; 4] = [&format, &io_surface, &metal, &colour];
        NSDictionary::from_slices(&keys, &values)
    }
}

/// 10-bit video-range NV12 (`x420`), with no colour conversion: the frame's
/// attachments describe its matrix, primaries and transfer function.
fn native_output_settings() -> Retained<NSDictionary<NSString, AnyObject>> {
    // SAFETY: CoreVideo's keys are CFStrings, toll-free bridged to NSString.
    unsafe {
        let key = |key: &CFString| -> &NSString { &*(key as *const CFString).cast::<NSString>() };
        let format = NSNumber::new_u32(kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange);
        let io_surface = NSDictionary::<NSString, AnyObject>::new();
        let metal = NSNumber::new_bool(true);
        let keys = [
            key(kCVPixelBufferPixelFormatTypeKey),
            key(kCVPixelBufferIOSurfacePropertiesKey),
            key(kCVPixelBufferMetalCompatibilityKey),
        ];
        let values: [&AnyObject; 3] = [&format, &io_surface, &metal];
        NSDictionary::from_slices(&keys, &values)
    }
}

/// Watches the item's end and playback failures. The blocks run on the main
/// queue; they note what happened and let the session act on it at once
/// when it is not busy, or at the next poll.
fn observe(
    inner: &Rc<RefCell<Inner>>,
    notes: &Rc<Notes>,
) -> Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>> {
    let item = inner.borrow().item.clone();
    let center = NSNotificationCenter::defaultCenter();
    let queue = NSOperationQueue::mainQueue();
    let mut observers = Vec::new();

    let wake = |weak: Weak<RefCell<Inner>>| {
        if let Some(inner) = weak.upgrade()
            && let Ok(mut inner) = inner.try_borrow_mut()
        {
            inner.poll();
        }
    };

    let (weak, ended_notes) = (Rc::downgrade(inner), notes.clone());
    let on_end = RcBlock::new(move |_: NonNull<NSNotification>| {
        ended_notes.ended.set(true);
        wake(weak.clone());
    });
    let (weak, failed_notes) = (Rc::downgrade(inner), notes.clone());
    let on_failure = RcBlock::new(move |note: NonNull<NSNotification>| {
        // SAFETY: the notification is live for the duration of the block,
        // and the key is a constant NSString.
        let error = unsafe {
            note.as_ref()
                .userInfo()
                .and_then(|info| info.objectForKey(AVPlayerItemFailedToPlayToEndTimeErrorKey))
                .and_then(|error| error.downcast::<NSError>().ok())
        };
        let error = match error {
            Some(error) => errors::from_ns_error(&error),
            None => {
                gpui_av::Error::new(gpui_av::ErrorKind::Other, "playback stopped before the end")
            }
        };
        *failed_notes.failed.borrow_mut() = Some(error);
        wake(weak.clone());
    });

    // SAFETY: the names are constant NSStrings, the object is the item the
    // blocks are about, and the blocks take an NSNotification.
    unsafe {
        let item: &AnyObject = &item;
        observers.push(center.addObserverForName_object_queue_usingBlock(
            Some(AVPlayerItemDidPlayToEndTimeNotification),
            Some(item),
            Some(&queue),
            &on_end,
        ));
        observers.push(center.addObserverForName_object_queue_usingBlock(
            Some(AVPlayerItemFailedToPlayToEndTimeNotification),
            Some(item),
            Some(&queue),
            &on_failure,
        ));
    }
    observers
}

fn cm_time(position: Duration) -> CMTime {
    // SAFETY: a pure CoreMedia constructor.
    unsafe { CMTime::with_seconds(position.as_secs_f64(), 90_000) }
}

impl Inner {
    fn report(&mut self, state: PlaybackState) {
        if self.reported.as_ref() != Some(&state) {
            self.reported = Some(state.clone());
            self.events.state(state);
        }
    }

    fn fail(&mut self, error: gpui_av::Error) {
        if self.failed {
            return;
        }
        // AVFoundation saw only the local server's 502; the server knows
        // what really went wrong.
        let error = match self.remuxed.as_ref().and_then(|server| server.failure()) {
            Some(failure) => super::remux_failure(failure),
            None => error,
        };
        self.failed = true;
        // SAFETY: plain AVPlayer commands.
        unsafe { self.player.pause() };
        self.awaiting_frame = None;
        match &self.http {
            Some(http) if probe::worth_probing(&error) => {
                // The report follows when the server answers.
                self.reported = Some(PlaybackState::Failed(error.clone()));
                probe::report(http, error, self.events.clone());
            }
            _ => self.report(PlaybackState::Failed(error)),
        }
    }

    /// Brings the reported state in line with the player.
    fn poll(&mut self) {
        if self.failed {
            return;
        }
        let failure = self.notes.failed.borrow_mut().take();
        if let Some(error) = failure {
            self.fail(error);
            return;
        }
        // SAFETY: plain property reads on the main thread.
        let status = unsafe { self.item.status() };
        if status == AVPlayerItemStatus::Failed {
            // SAFETY: plain property reads.
            let error = unsafe { self.item.error().or_else(|| self.player.error()) };
            let error = match error {
                Some(error) => errors::from_ns_error(&error),
                None => gpui_av::Error::new(gpui_av::ErrorKind::Other, "the media failed to load"),
            };
            self.fail(error);
            return;
        }
        if status != AVPlayerItemStatus::ReadyToPlay {
            return;
        }
        if self.loaded.is_none() {
            self.on_loaded();
            if self.failed {
                return;
            }
        }
        if self.notes.ended.replace(false) {
            self.on_end();
        }

        let state = if self.ended {
            PlaybackState::Ended
        } else {
            // SAFETY: a plain property read.
            match unsafe { self.player.timeControlStatus() } {
                AVPlayerTimeControlStatus::Playing => PlaybackState::Playing,
                AVPlayerTimeControlStatus::WaitingToPlayAtSpecifiedRate => {
                    // A seek stalls the player for a moment; that is not
                    // buffering worth reporting.
                    if self.seek.in_flight.load(Ordering::Acquire)
                        && matches!(self.reported, Some(PlaybackState::Playing))
                    {
                        PlaybackState::Playing
                    } else {
                        PlaybackState::Buffering
                    }
                }
                _ if self.started => PlaybackState::Paused,
                _ => PlaybackState::Ready,
            }
        };
        self.report(state);
    }

    fn on_loaded(&mut self) {
        let mut loaded = info::read(&self.item);
        // AVFoundation tells less about HLS media than about a file before
        // the first frame; the remuxer read the file itself.
        if let Some(summary) = self.remuxed.as_ref().and_then(|server| server.summary()) {
            let hint = gpui_av::MediaInfo {
                duration: Some(summary.duration),
                natural_size: summary
                    .video_size
                    .map(|(width, height)| gpui::size(width, height)),
                has_video: summary.video_size.is_some(),
                has_audio: Some(summary.has_audio),
                ..Default::default()
            };
            info::fill_from(&mut loaded.info, &hint);
        }
        if let Some(codec) = &loaded.undecodable {
            self.fail(gpui_av::Error::new(
                gpui_av::ErrorKind::Unsupported,
                format!("this Mac has no decoder for the {codec} track"),
            ));
            return;
        }
        if !loaded.info.has_video {
            self.awaiting_frame = None;
        }
        if loaded
            .info
            .rotation
            .is_some_and(|rotation| rotation != Rotation::None)
        {
            // AVPlayerItemVideoOutput hands out frames as stored; a video
            // composition built from the asset applies the track's rotation.
            // SAFETY: the asset is loaded, so this does not block for long.
            #[allow(deprecated)]
            unsafe {
                let composition =
                    AVVideoComposition::videoCompositionWithPropertiesOfAsset(&self.item.asset());
                self.item.setVideoComposition(Some(&composition));
            }
        }
        let groups = Groups::read(&self.item);
        // Subtitles inside the media start off, as on every backend;
        // AVPlayer would otherwise pick some by the system's settings.
        groups.select(&self.item, TrackKind::Text, None);
        for (kind, index) in std::mem::take(&mut self.pending_tracks) {
            groups.select(&self.item, kind, index);
        }
        loaded.info.tracks = groups.tracks();
        self.groups = Some(groups);
        self.refresh_tracks(&mut loaded.info);
        self.events.info(loaded.info.clone());
        self.loaded = Some(loaded);
        if let Some(position) = self.pending_seek.take() {
            self.seek_to(position, true);
        }
    }

    /// Reads which audio and text tracks are on into `info`.
    fn refresh_tracks(&self, info: &mut gpui_av::MediaInfo) {
        if let Some(groups) = &self.groups {
            info.audio_track = groups.selected(&self.item, TrackKind::Audio);
            info.text_track = groups.selected(&self.item, TrackKind::Text);
        }
    }

    fn select_track(&mut self, kind: TrackKind, index: Option<usize>) {
        let Some(groups) = &self.groups else {
            self.pending_tracks.push((kind, index));
            return;
        };
        groups.select(&self.item, kind, index);
        if kind == TrackKind::Text {
            self.legible.clear();
        }
        let Some(mut info) = self.loaded.as_ref().map(|loaded| loaded.info.clone()) else {
            return;
        };
        self.refresh_tracks(&mut info);
        if let Some(loaded) = &mut self.loaded {
            loaded.info = info.clone();
        }
        self.events.info(info);
    }

    fn on_end(&mut self) {
        if self.looping {
            self.seek_to(Duration::ZERO, false);
            // SAFETY: a plain AVPlayer command.
            unsafe { self.player.play() };
        } else {
            self.ended = true;
        }
    }

    fn duration(&self) -> Option<Duration> {
        self.loaded.as_ref().and_then(|loaded| loaded.info.duration)
    }

    /// Seeks to exactly `position`. `announce` sends `Seeked` when done.
    fn seek_to(&mut self, position: Duration, announce: bool) {
        let position = match self.duration() {
            Some(duration) => position.min(duration),
            None => position,
        };
        let generation = self.seek.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.seek.in_flight.store(true, Ordering::Release);
        self.frame_time = None;
        self.cursor = None;
        self.drift_from = Some(Instant::now() + SETTLE);
        if self
            .loaded
            .as_ref()
            .is_some_and(|loaded| loaded.info.has_video)
        {
            self.awaiting_frame = Some(Instant::now());
        }
        let seek = self.seek.clone();
        let events = self.events.clone();
        let done = RcBlock::new(move |finished: Bool| {
            // A newer seek replaces this one; only the latest one reports.
            if seek.generation.load(Ordering::Acquire) != generation {
                return;
            }
            seek.in_flight.store(false, Ordering::Release);
            if finished.as_bool() && announce {
                events.seeked(position);
            }
        });
        // SAFETY: a plain AVPlayer command; the block only touches Send
        // values, since AVFoundation does not promise which thread calls it.
        unsafe {
            self.player
                .seekToTime_toleranceBefore_toleranceAfter_completionHandler(
                    cm_time(position),
                    kCMTimeZero,
                    kCMTimeZero,
                    &done,
                );
        }
    }

    /// Takes the frame due now from the output, if there is a new one, and
    /// counts frames that were due and never shown.
    fn take_frame(&mut self) {
        // SAFETY: plain reads on the output; the out pointer is valid.
        let (frame, shown_at) = unsafe {
            let host_time = CVGetCurrentHostTime() as i64;
            let item_time = self.output.itemTimeForMachAbsoluteTime(host_time);
            if !self.output.hasNewPixelBufferForItemTime(item_time) {
                return;
            }
            let mut shown_at = kCMTimeInvalid;
            let frame = self
                .output
                .copyPixelBufferForItemTime_itemTimeForDisplay(item_time, &mut shown_at);
            (frame, info::duration(shown_at))
        };
        let Some(frame) = frame else { return };
        // During a seek the output can still hand out frames from before
        // it; none of them start a run of frames to count drops against.
        let seeking = self.seek.in_flight.load(Ordering::Acquire);
        // The binding returns `Retained`, GPUI takes `CFRetained`: the same
        // +1 reference to a CoreVideo buffer, under two owners' names.
        // SAFETY: ownership of the one reference moves from one to the other.
        let frame: CFRetained<CVPixelBuffer> =
            unsafe { CFRetained::from_raw(NonNull::new_unchecked(Retained::into_raw(frame))) };
        self.count(shown_at);
        self.frame = Some(frame);
        self.frame_time = if seeking { None } else { shown_at };
        if seeking {
            self.cursor = None;
        }
        self.awaiting_frame = None;
    }

    fn count(&mut self, shown_at: Option<Duration>) {
        self.stats.frames_presented += 1;
        // SAFETY: a plain property read.
        let playing =
            unsafe { self.player.timeControlStatus() } == AVPlayerTimeControlStatus::Playing;
        let Some(now) = shown_at else {
            self.cursor = None;
            return;
        };
        if playing && self.drift_from.is_some_and(|from| Instant::now() >= from) {
            // SAFETY: a plain property read.
            let clock = info::duration(unsafe { self.player.currentTime() }).unwrap_or_default();
            let drift = now.abs_diff(clock);
            self.max_drift = Some(self.max_drift.map_or(drift, |max| max.max(drift)));
        }
        match self.frame_time {
            Some(previous) if playing && now > previous => {
                self.stats.frames_dropped += self.skipped_between(previous, now);
            }
            _ => self.cursor = None,
        }
    }

    /// How many frames lie strictly between two shown frames.
    fn skipped_between(&mut self, previous: Duration, now: Duration) -> u64 {
        let Some(track) = self
            .loaded
            .as_ref()
            .and_then(|loaded| loaded.sample_track.clone())
        else {
            // No sample list: assume a constant frame rate.
            let Some(frame) = self
                .loaded
                .as_ref()
                .and_then(|loaded| loaded.frame_duration)
            else {
                return 0;
            };
            let frames = ((now - previous).as_secs_f64() / frame.as_secs_f64()).round() as u64;
            return frames.saturating_sub(1);
        };
        // SAFETY: cursor calls on a loaded track, on the main thread.
        unsafe {
            let cursor = match self.cursor.take() {
                Some(cursor) => cursor,
                None => match track.makeSampleCursorWithPresentationTimeStamp(cm_time(previous)) {
                    Some(cursor) => cursor,
                    None => return 0,
                },
            };
            let mut skipped = 0;
            // Bounded, in case the times never meet (a damaged index).
            for _ in 0..10_000 {
                if cursor.stepInPresentationOrderByCount(1) != 1 {
                    break;
                }
                let Some(at) = info::duration(cursor.presentationTimeStamp()) else {
                    break;
                };
                if at + SAME_FRAME >= now {
                    break;
                }
                if at > previous + SAME_FRAME {
                    skipped += 1;
                }
            }
            self.cursor = Some(cursor);
            skipped
        }
    }
}

impl Session for AvSession {
    fn play(&mut self) {
        let mut inner = self.inner.borrow_mut();
        if inner.ended {
            inner.ended = false;
            inner.seek_to(Duration::ZERO, false);
        }
        inner.started = true;
        inner.drift_from = Some(Instant::now() + SETTLE);
        // SAFETY: a plain AVPlayer command.
        unsafe { inner.player.play() };
        inner.poll();
    }

    fn pause(&mut self) {
        let mut inner = self.inner.borrow_mut();
        inner.started = true;
        // SAFETY: a plain AVPlayer command.
        unsafe { inner.player.pause() };
        inner.poll();
    }

    fn seek(&mut self, position: Duration) {
        let mut inner = self.inner.borrow_mut();
        inner.ended = false;
        inner.legible.clear();
        if inner.loaded.is_none() {
            inner.pending_seek = Some(position);
        } else {
            inner.seek_to(position, true);
        }
        inner.poll();
    }

    fn set_volume(&mut self, volume: f32) {
        // SAFETY: a plain AVPlayer property.
        unsafe { self.inner.borrow().player.setVolume(volume) };
    }

    fn set_muted(&mut self, muted: bool) {
        // SAFETY: a plain AVPlayer property.
        unsafe { self.inner.borrow().player.setMuted(muted) };
    }

    fn set_looping(&mut self, looping: bool) {
        self.inner.borrow_mut().looping = looping;
    }

    fn position(&self) -> Duration {
        let inner = self.inner.borrow();
        if inner.loaded.is_none() {
            return inner.pending_seek.unwrap_or_default();
        }
        // SAFETY: a plain property read.
        info::duration(unsafe { inner.player.currentTime() }).unwrap_or_default()
    }

    fn buffered(&self) -> Vec<std::ops::Range<Duration>> {
        let inner = self.inner.borrow();
        if inner.loaded.is_none() {
            return Vec::new();
        }
        // SAFETY: a plain property read; AVPlayerItem documents each value
        // as holding a CMTimeRange.
        let ranges = unsafe { inner.item.loadedTimeRanges() };
        ranges
            .iter()
            .filter_map(|value| {
                // SAFETY: as above.
                let range = unsafe { value.CMTimeRangeValue() };
                let start = info::duration(range.start)?;
                let end = start + info::duration(range.duration)?;
                (start < end).then_some(start..end)
            })
            .collect()
    }

    fn select_track(&mut self, kind: TrackKind, index: Option<usize>) {
        self.inner.borrow_mut().select_track(kind, index);
    }

    fn text(&self) -> Option<SharedString> {
        let inner = self.inner.borrow();
        inner.loaded.as_ref()?.info.text_track?;
        inner.legible.text().map(SharedString::from)
    }

    fn stats(&self) -> Stats {
        let inner = self.inner.borrow();
        match inner.max_drift {
            Some(drift) => inner.stats.with_max_drift(drift),
            None => inner.stats,
        }
    }

    fn paint(&mut self, placement: Placement, window: &mut Window, _cx: &mut App) {
        let mut inner = self.inner.borrow_mut();
        if inner.failed {
            return;
        }
        inner.take_frame();
        if let Some(frame) = &inner.frame {
            window.paint_surface(placement.bounds, frame.clone());
        }
        if inner
            .awaiting_frame
            .is_some_and(|since| since.elapsed() < FRAME_WAIT)
        {
            window.request_animation_frame();
        }
    }
}

impl Drop for AvSession {
    fn drop(&mut self) {
        let center = NSNotificationCenter::defaultCenter();
        for observer in self.observers.drain(..) {
            // SAFETY: each observer was returned by this center.
            unsafe { center.removeObserver(observer.as_ref()) };
        }
        let inner = self.inner.borrow();
        // SAFETY: plain AVFoundation commands on the main thread.
        unsafe {
            inner.player.pause();
            inner.item.removeOutput(&inner.output);
            inner.legible.detach(&inner.item);
            inner.player.replaceCurrentItemWithPlayerItem(None);
        }
    }
}
