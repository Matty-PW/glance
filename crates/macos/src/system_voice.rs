//! The macOS system voice: `AVSpeechSynthesizer` through the objc2 bindings.
//!
//! No model download, no phonemizer, no extra permission. Quality depends on
//! the voices installed in System Settings (the "Premium" voices are good).
//!
//! How it is put together, and why:
//! - `AVSpeechSynthesizer` is not `Send`, so one dedicated thread creates it
//!   and is the only thread that ever touches it. [`SystemVoice`] just sends
//!   that thread commands.
//! - macOS delivers the delegate callbacks (started / finished / cancelled)
//!   on the **main thread's run loop**, whichever thread the synthesizer
//!   lives on. Measured in M1: with the main thread blocked, no callback ever
//!   arrives. So the process must keep its main run loop turning. The app
//!   does that anyway from M2; the CLI uses [`run_main_loop_until`].
//!
//! `unsafe` here: every call into the generated Objective-C bindings is an
//! `unsafe fn`.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_avf_audio::{
    AVSpeechBoundary, AVSpeechSynthesisVoice, AVSpeechSynthesisVoiceQuality, AVSpeechSynthesizer,
    AVSpeechSynthesizerDelegate, AVSpeechUtterance,
};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSRunLoop, NSString};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use glance_tts::{SpeechEvent, TextToSpeech, TtsError, UtteranceId};

/// Settings for the system voice.
#[derive(Debug, Clone)]
pub struct SystemVoiceConfig {
    /// A voice identifier (see [`installed_voices`]) or a language code such
    /// as `en-GB`. `None` uses the system default voice.
    pub voice: Option<String>,
    /// Speaking rate from 0.0 (slowest) to 1.0 (fastest). `None` keeps the
    /// system default, which is 0.5.
    pub rate: Option<f32>,
    /// Volume from 0.0 (silent) to 1.0.
    pub volume: f32,
}

impl Default for SystemVoiceConfig {
    fn default() -> Self {
        Self {
            voice: None,
            rate: None,
            volume: 1.0,
        }
    }
}

/// One voice installed on this Mac.
#[derive(Debug, Clone)]
pub struct VoiceInfo {
    pub identifier: String,
    pub name: String,
    pub language: String,
    /// "default", "enhanced" or "premium".
    pub quality: &'static str,
}

/// State shared between the speech thread and the delegate callbacks.
struct Shared {
    events: UnboundedSender<SpeechEvent>,
    /// Utterances handed to the synthesizer and not yet finished, keyed by
    /// the address of the `AVSpeechUtterance` object. The synthesizer keeps
    /// each object alive until its final callback, so addresses are unique
    /// for as long as an entry exists.
    pending: Mutex<HashMap<usize, UtteranceId>>,
}

impl Shared {
    fn pending(&self) -> MutexGuard<'_, HashMap<usize, UtteranceId>> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn started(&self, utterance: &AVSpeechUtterance) {
        if let Some(&id) = self.pending().get(&address(utterance)) {
            let _ = self.events.send(SpeechEvent::Started(id));
        }
    }

    fn ended(&self, utterance: &AVSpeechUtterance, event: fn(UtteranceId) -> SpeechEvent) {
        if let Some(id) = self.pending().remove(&address(utterance)) {
            let _ = self.events.send(event(id));
        }
    }
}

fn address(utterance: &AVSpeechUtterance) -> usize {
    std::ptr::from_ref(utterance) as usize
}

struct DelegateIvars {
    shared: Arc<Shared>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `SpeechDelegate` does not implement `Drop`.
    // - Its only state is an `Arc<Shared>`, which is `Send + Sync`, as the
    //   delegate protocol requires.
    #[unsafe(super(NSObject))]
    #[ivars = DelegateIvars]
    struct SpeechDelegate;

    unsafe impl NSObjectProtocol for SpeechDelegate {}

    unsafe impl AVSpeechSynthesizerDelegate for SpeechDelegate {
        #[unsafe(method(speechSynthesizer:didStartSpeechUtterance:))]
        fn did_start(&self, _synthesizer: &AVSpeechSynthesizer, utterance: &AVSpeechUtterance) {
            self.ivars().shared.started(utterance);
        }

        #[unsafe(method(speechSynthesizer:didFinishSpeechUtterance:))]
        fn did_finish(&self, _synthesizer: &AVSpeechSynthesizer, utterance: &AVSpeechUtterance) {
            self.ivars().shared.ended(utterance, SpeechEvent::Finished);
        }

        #[unsafe(method(speechSynthesizer:didCancelSpeechUtterance:))]
        fn did_cancel(&self, _synthesizer: &AVSpeechSynthesizer, utterance: &AVSpeechUtterance) {
            self.ivars().shared.ended(utterance, SpeechEvent::Cancelled);
        }
    }
);

impl SpeechDelegate {
    fn new(shared: Arc<Shared>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(DelegateIvars { shared });
        // SAFETY: plain `-[NSObject init]` on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }
}

enum Command {
    Speak {
        id: UtteranceId,
        text: String,
    },
    Stop,
    /// From the next sentence on (settings window).
    SetVoice(Option<String>),
    SetRate(Option<f32>),
}

/// The macOS system voice.
pub struct SystemVoice {
    commands: mpsc::Sender<Command>,
    next_id: AtomicU64,
}

impl SystemVoice {
    /// Start the speech thread. Events arrive on the returned receiver, but
    /// only while the process's main run loop is running (see module docs).
    pub fn new(config: SystemVoiceConfig) -> (Self, UnboundedReceiver<SpeechEvent>) {
        let (events, receiver) = unbounded_channel();
        let (commands, inbox) = mpsc::channel();
        let shared = Arc::new(Shared {
            events,
            pending: Mutex::new(HashMap::new()),
        });
        std::thread::Builder::new()
            .name("glance-speech".into())
            .spawn(move || speech_thread(&inbox, &shared, &config))
            .expect("could not start the speech thread");
        (
            Self {
                commands,
                next_id: AtomicU64::new(1),
            },
            receiver,
        )
    }
}

impl TextToSpeech for SystemVoice {
    fn speak(&self, text: &str) -> Result<UtteranceId, TtsError> {
        let id = UtteranceId(self.next_id.fetch_add(1, Ordering::Relaxed));
        self.commands
            .send(Command::Speak {
                id,
                text: text.to_owned(),
            })
            .map_err(|_| TtsError::EngineGone)?;
        Ok(id)
    }

    fn stop(&self) {
        let _ = self.commands.send(Command::Stop);
    }
}

impl SystemVoice {
    /// Use another voice (identifier or language) from the next sentence.
    pub fn set_voice(&self, voice: Option<String>) {
        let _ = self.commands.send(Command::SetVoice(voice));
    }

    /// Speaking rate, 0.0 to 1.0, from the next sentence.
    pub fn set_rate(&self, rate: Option<f32>) {
        let _ = self.commands.send(Command::SetRate(rate));
    }
}

/// Body of the speech thread. Ends, silencing any speech, when the owning
/// [`SystemVoice`] is dropped.
fn speech_thread(
    inbox: &mpsc::Receiver<Command>,
    shared: &Arc<Shared>,
    config: &SystemVoiceConfig,
) {
    // SAFETY (for every call in this function): these are ordinary
    // Objective-C messages to objects created and used only on this thread,
    // with arguments of the types the bindings declare.
    let synthesizer = unsafe { AVSpeechSynthesizer::new() };
    let delegate = SpeechDelegate::new(Arc::clone(shared));
    // The synthesizer holds its delegate weakly; `delegate` lives until the
    // end of this function, after the final `stop` below.
    unsafe { synthesizer.setDelegate(Some(ProtocolObject::from_ref(&*delegate))) };
    let mut voice = config.voice.as_deref().and_then(find_voice);
    let mut rate = config.rate;

    let silence = |synthesizer: &AVSpeechSynthesizer| {
        unsafe { synthesizer.stopSpeakingAtBoundary(AVSpeechBoundary::Immediate) };
        // Report everything still outstanding ourselves, so callers get
        // exactly one `Cancelled` per utterance whatever macOS reports later.
        let mut dropped: Vec<UtteranceId> = shared.pending().drain().map(|(_, id)| id).collect();
        dropped.sort();
        for id in dropped {
            let _ = shared.events.send(SpeechEvent::Cancelled(id));
        }
    };

    for command in inbox {
        match command {
            Command::Speak { id, text } => {
                let utterance = unsafe {
                    AVSpeechUtterance::speechUtteranceWithString(&NSString::from_str(&text))
                };
                unsafe {
                    if let Some(voice) = &voice {
                        utterance.setVoice(Some(voice));
                    }
                    if let Some(rate) = rate {
                        utterance.setRate(rate.clamp(0.0, 1.0));
                    }
                    utterance.setVolume(config.volume.clamp(0.0, 1.0));
                }
                shared.pending().insert(address(&utterance), id);
                unsafe { synthesizer.speakUtterance(&utterance) };
            }
            Command::Stop => silence(&synthesizer),
            Command::SetVoice(wanted) => voice = wanted.as_deref().and_then(find_voice),
            Command::SetRate(new_rate) => rate = new_rate,
        }
    }
    silence(&synthesizer);
}

fn find_voice(wanted: &str) -> Option<Retained<AVSpeechSynthesisVoice>> {
    let wanted = NSString::from_str(wanted);
    // SAFETY: class methods taking an NSString; both return nil when there is
    // no such voice.
    unsafe {
        AVSpeechSynthesisVoice::voiceWithIdentifier(&wanted)
            .or_else(|| AVSpeechSynthesisVoice::voiceWithLanguage(Some(&wanted)))
    }
}

/// Every voice installed on this Mac.
pub fn installed_voices() -> Vec<VoiceInfo> {
    // SAFETY: a class method returning an array, then read-only property
    // getters on its elements.
    let voices = unsafe { AVSpeechSynthesisVoice::speechVoices() };
    (0..voices.count())
        .map(|index| unsafe {
            let voice = voices.objectAtIndex(index);
            let quality = voice.quality();
            VoiceInfo {
                identifier: voice.identifier().to_string(),
                name: voice.name().to_string(),
                language: voice.language().to_string(),
                quality: if quality == AVSpeechSynthesisVoiceQuality::Premium {
                    "premium"
                } else if quality == AVSpeechSynthesisVoiceQuality::Enhanced {
                    "enhanced"
                } else {
                    "default"
                },
            }
        })
        .collect()
}

/// Keep the calling thread's run loop turning until `done` returns true.
///
/// Call this on the **main thread** of a command-line program that uses
/// [`SystemVoice`]; without it no speech events are ever delivered. A GUI
/// event loop makes this unnecessary.
pub fn run_main_loop_until(mut done: impl FnMut() -> bool) {
    let run_loop = NSRunLoop::currentRunLoop();
    while !done() {
        let slice = NSDate::dateWithTimeIntervalSinceNow(0.02);
        // SAFETY: `NSDefaultRunLoopMode` is a constant exported by Foundation.
        let ran = run_loop.runMode_beforeDate(unsafe { NSDefaultRunLoopMode }, &slice);
        if !ran {
            // No input sources yet: the call returned at once. Avoid spinning.
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
