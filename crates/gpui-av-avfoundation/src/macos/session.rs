//! One piece of media playing through `AVPlayer`.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use block2::RcBlock;
use gpui::{App, Task, Window};
use gpui_av::{EventSink, Placement, PlaybackState, Rotation, Session, Stats};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{AnyThread, MainThreadMarker};
use objc2_av_foundation::{
    AVPlayer, AVPlayerActionAtItemEnd, AVPlayerItem, AVPlayerItemDidPlayToEndTimeNotification,
    AVPlayerItemFailedToPlayToEndTimeErrorKey, AVPlayerItemFailedToPlayToEndTimeNotification,
    AVPlayerItemStatus, AVPlayerItemVideoOutput, AVPlayerTimeControlStatus, AVURLAsset,
    AVVideoColorPrimaries_ITU_R_709_2, AVVideoColorPrimariesKey, AVVideoColorPropertiesKey,
    AVVideoComposition, AVVideoTransferFunction_ITU_R_709_2, AVVideoTransferFunctionKey,
    AVVideoYCbCrMatrix_ITU_R_601_4, AVVideoYCbCrMatrixKey,
};
use objc2_core_foundation::{CFRetained, CFString};
use objc2_core_media::{CMTime, kCMTimeInvalid, kCMTimeZero};
use objc2_core_video::{
    CVGetCurrentHostTime, CVPixelBuffer, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferMetalCompatibilityKey, kCVPixelBufferPixelFormatTypeKey,
    kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
};
use objc2_foundation::{
    NSDictionary, NSError, NSNotification, NSNotificationCenter, NSNumber, NSObjectProtocol,
    NSOperationQueue, NSString,
};

use super::errors;
use super::info::{self, Loaded};
use super::probe::{self, HttpSource};

/// How often the session checks the player for state changes. AVFoundation
/// reports most of them through key-value observing, which needs an
/// Objective-C class; polling a few properties is cheaper to maintain.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// How long the session keeps asking for window frames while it waits for
/// a picture that has not arrived (the first frame, or the frame after a
/// seek while paused).
const FRAME_WAIT: Duration = Duration::from_secs(3);

pub(crate) struct AvSession {
    inner: Rc<RefCell<Inner>>,
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
    /// Waiting for a new picture since then.
    awaiting_frame: Option<Instant>,
    stats: Stats,
}

impl AvSession {
    pub(crate) fn new(
        asset: Retained<AVURLAsset>,
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
            (player, item, output)
        };
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
            awaiting_frame: Some(Instant::now()),
            stats: Stats::default(),
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
        }
    }
}

/// The settings for the frames GPUI draws. GPUI's Metal surface takes only
/// full-range NV12 (`420f`), backed by an IOSurface, and converts it to
/// RGB with the BT.601 matrix, so the output asks AVFoundation to convert
/// every frame to exactly that.
fn output_settings() -> Retained<NSDictionary<NSString, AnyObject>> {
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
        let loaded = info::read(&self.item);
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
        if loaded.info.rotation != Rotation::None {
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
        self.events.info(loaded.info.clone());
        self.loaded = Some(loaded);
        if let Some(position) = self.pending_seek.take() {
            self.seek_to(position, true);
        }
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
        self.awaiting_frame = None;
    }

    fn count(&mut self, shown_at: Option<Duration>) {
        self.stats.frames_presented += 1;
        let frame_duration = self
            .loaded
            .as_ref()
            .and_then(|loaded| loaded.frame_duration);
        let (Some(previous), Some(now), Some(frame_duration)) =
            (self.frame_time, shown_at, frame_duration)
        else {
            return;
        };
        // SAFETY: a plain property read.
        let playing =
            unsafe { self.player.timeControlStatus() } == AVPlayerTimeControlStatus::Playing;
        if !playing || now <= previous {
            return;
        }
        let frames = (now - previous).as_secs_f64() / frame_duration.as_secs_f64();
        let missed = frames.round() as u64;
        if missed > 1 {
            self.stats.frames_dropped += missed - 1;
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

    fn stats(&self) -> Stats {
        self.inner.borrow().stats
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
            inner.player.replaceCurrentItemWithPlayerItem(None);
        }
    }
}
