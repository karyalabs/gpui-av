//! Audio and text tracks inside the media: AVFoundation's media selection
//! groups, and the text of the subtitles shown, through a legible output.
//!
//! The session draws frames from a video output, not an `AVPlayerLayer`,
//! so AVFoundation never draws subtitles itself; the legible output hands
//! their text over and gpui-av's `captions()` band shows it.

use std::sync::{Arc, Mutex};

use dispatch2::DispatchQueue;
use gpui_av::{Track, TrackKind};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_av_foundation::{
    AVMediaCharacteristicAudible, AVMediaCharacteristicLegible, AVMediaSelectionGroup,
    AVPlayerItem, AVPlayerItemLegibleOutput, AVPlayerItemLegibleOutputPushDelegate,
    AVPlayerItemOutputPushDelegate,
};
use objc2_core_media::CMTime;
use objc2_foundation::{NSArray, NSAttributedString, NSObject, NSObjectProtocol};

/// The media's audio and text groups, read once the item is ready.
pub(crate) struct Groups {
    audio: Option<Retained<AVMediaSelectionGroup>>,
    text: Option<Retained<AVMediaSelectionGroup>>,
}

impl Groups {
    /// Reads the groups. Call once the item is ready to play: the asset is
    /// loaded then, so the lookup does not wait on the media.
    pub(crate) fn read(item: &AVPlayerItem) -> Self {
        // SAFETY: constant NSStrings exported by AVFoundation, and lookups
        // on a loaded asset.
        #[allow(deprecated)]
        unsafe {
            let asset = item.asset();
            let group = |characteristic: Option<&_>| {
                characteristic.and_then(|characteristic| {
                    asset.mediaSelectionGroupForMediaCharacteristic(characteristic)
                })
            };
            Self {
                audio: group(AVMediaCharacteristicAudible),
                text: group(AVMediaCharacteristicLegible),
            }
        }
    }

    fn group(&self, kind: TrackKind) -> Option<&AVMediaSelectionGroup> {
        match kind {
            TrackKind::Audio => self.audio.as_deref(),
            TrackKind::Text => self.text.as_deref(),
            _ => None,
        }
    }

    /// Every audio and text option, as gpui-av tracks.
    pub(crate) fn tracks(&self) -> Vec<Track> {
        let mut tracks = Vec::new();
        for kind in [TrackKind::Audio, TrackKind::Text] {
            let Some(group) = self.group(kind) else {
                continue;
            };
            // SAFETY: property reads on a loaded group and its options.
            unsafe {
                for (index, option) in group.options().iter().enumerate() {
                    let mut track = Track::new(kind, index);
                    if let Some(language) = option.extendedLanguageTag() {
                        track = track.with_language(language.to_string());
                    }
                    let title = option.displayName().to_string();
                    if !title.is_empty() {
                        track = track.with_title(title);
                    }
                    tracks.push(track);
                }
            }
        }
        tracks
    }

    /// The index of the option on in the group of this kind.
    pub(crate) fn selected(&self, item: &AVPlayerItem, kind: TrackKind) -> Option<usize> {
        let group = self.group(kind)?;
        // SAFETY: property reads on the item and a group of its asset.
        unsafe {
            let chosen = item
                .currentMediaSelection()
                .selectedMediaOptionInMediaSelectionGroup(group)?;
            group
                .options()
                .iter()
                .position(|option| option.isEqual(Some(&chosen)))
        }
    }

    /// Turns on the option at `index` of this kind, or turns the kind off
    /// for `None` where the group allows nothing on (text does, audio
    /// usually does not). An index past the end does nothing.
    pub(crate) fn select(&self, item: &AVPlayerItem, kind: TrackKind, index: Option<usize>) {
        let Some(group) = self.group(kind) else {
            return;
        };
        // SAFETY: selecting an option of a group of the item's asset, on the
        // main thread.
        unsafe {
            match index {
                Some(index) => {
                    let options = group.options();
                    if index < options.count() {
                        let option = options.objectAtIndex(index);
                        item.selectMediaOption_inMediaSelectionGroup(Some(&option), group);
                    }
                }
                None if group.allowsEmptySelection() => {
                    item.selectMediaOption_inMediaSelectionGroup(None, group);
                }
                None => {}
            }
        }
    }
}

/// The subtitle text on screen now, shared with the legible output's
/// delegate.
pub(crate) type Shown = Arc<Mutex<String>>;

define_class!(
    /// Takes the legible output's strings and keeps the latest as the text
    /// on screen.
    #[unsafe(super(NSObject))]
    #[name = "GpuiAvLegibleText"]
    #[ivars = Shown]
    struct LegibleText;

    unsafe impl NSObjectProtocol for LegibleText {}

    unsafe impl AVPlayerItemOutputPushDelegate for LegibleText {}

    unsafe impl AVPlayerItemLegibleOutputPushDelegate for LegibleText {
        #[unsafe(method(legibleOutput:didOutputAttributedStrings:nativeSampleBuffers:forItemTime:))]
        fn did_output(
            &self,
            _output: &AVPlayerItemLegibleOutput,
            strings: &NSArray<NSAttributedString>,
            _native: &NSArray,
            _time: CMTime,
        ) {
            // An empty array clears the text.
            let text = strings
                .iter()
                .map(|string| string.string().to_string())
                .collect::<Vec<_>>()
                .join("\n");
            if let Ok(mut shown) = self.ivars().lock() {
                *shown = text;
            }
        }
    }
);

/// A legible output on an item, with its delegate.
pub(crate) struct Legible {
    output: Retained<AVPlayerItemLegibleOutput>,
    /// The output holds its delegate weakly.
    _delegate: Retained<LegibleText>,
    shown: Shown,
}

impl Legible {
    pub(crate) fn attach(item: &AVPlayerItem) -> Self {
        let shown = Shown::default();
        let delegate = LegibleText::alloc().set_ivars(shown.clone());
        // SAFETY: NSObject's init on a freshly allocated object.
        let delegate: Retained<LegibleText> = unsafe { msg_send![super(delegate), init] };
        // SAFETY: creating an output and adding it to the item on the main
        // thread; the delegate is called on the main queue.
        let output = unsafe {
            let output = AVPlayerItemLegibleOutput::init(AVPlayerItemLegibleOutput::alloc());
            output.setSuppressesPlayerRendering(true);
            output.setDelegate_queue(
                Some(ProtocolObject::from_ref(&*delegate)),
                Some(DispatchQueue::main()),
            );
            item.addOutput(&output);
            output
        };
        Self {
            output,
            _delegate: delegate,
            shown,
        }
    }

    /// The text on screen, if any.
    pub(crate) fn text(&self) -> Option<String> {
        let shown = self.shown.lock().ok()?;
        (!shown.is_empty()).then(|| shown.clone())
    }

    /// Forgets the text on screen, after a seek or turning text off.
    pub(crate) fn clear(&self) {
        if let Ok(mut shown) = self.shown.lock() {
            shown.clear();
        }
    }

    pub(crate) fn detach(&self, item: &AVPlayerItem) {
        // SAFETY: removing an output this item holds, on the main thread.
        unsafe {
            self.output.setDelegate_queue(None, None);
            item.removeOutput(&self.output);
        }
    }
}
