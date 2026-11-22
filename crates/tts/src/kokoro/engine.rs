//! [`KokoroVoice`]: Kokoro behind [`TextToSpeech`].
//!
//! One engine thread owns the phonemizer and the ONNX session and
//! synthesizes sentences in order, so the next sentence is ready while the
//! current one plays. The speaker is shared, so [`TextToSpeech::stop`]
//! silences it at once from the caller's thread instead of waiting for a
//! synthesis in progress. A generation counter makes the engine drop
//! sentences queued before the stop.
//!
//! The output device is opened on the first sentence (or on
//! [`KokoroVoice::warm_up`]) and closed after a quiet period, so an idle
//! glance does not keep the audio hardware running.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::time::{Duration, Instant};

use glance_audio::{AudioClip, AudioOutput, PlaybackEvent, Speaker};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use super::model::{KokoroModel, SAMPLE_RATE};
use super::phonemizer::{Accent, Misaki, Phonemizer};
use super::voice::{VoiceChoice, VoiceStyle};
use crate::{SpeechEvent, TextToSpeech, TtsError, UtteranceId};

/// Everything needed to start Kokoro. All paths must already be verified
/// against `models.toml`.
#[derive(Debug, Clone)]
pub struct KokoroConfig {
    pub onnx: PathBuf,
    pub tokenizer: PathBuf,
    /// The voices model's directory (holding `voices/<id>.bin`).
    pub voices_dir: PathBuf,
    pub voice: VoiceChoice,
    /// 1.0 is Kokoro's normal pace; 0.5–2.0 is sensible.
    pub speed: f32,
    /// ONNX Runtime intra-op threads.
    pub threads: usize,
    /// Output gain, 0.0 (silent) to 1.0.
    pub volume: f32,
    /// Close the output device after this long with nothing to play.
    pub idle_close: Duration,
}

/// Load the style table for a voice or a blend.
pub fn load_voice(voices_dir: &Path, choice: &VoiceChoice) -> Result<VoiceStyle, TtsError> {
    let path = |id: &str| voices_dir.join("voices").join(format!("{id}.bin"));
    match choice {
        VoiceChoice::Single(id) => VoiceStyle::load(&path(id)),
        VoiceChoice::Blend(parts) => {
            let voices = parts
                .iter()
                .map(|(id, w)| Ok((VoiceStyle::load(&path(id))?, *w)))
                .collect::<Result<Vec<_>, TtsError>>()?;
            let refs: Vec<(&VoiceStyle, f32)> = voices.iter().map(|(v, w)| (v, *w)).collect();
            VoiceStyle::blend(&refs).map_err(TtsError::Engine)
        }
    }
}

enum Command {
    Speak {
        id: UtteranceId,
        text: String,
        generation: u64,
    },
    WarmUp,
    /// A new voice (or blend) from the next sentence; the phonemizer follows
    /// its accent.
    SetVoice(Box<VoiceStyle>, Accent),
}

struct Shared {
    events: UnboundedSender<SpeechEvent>,
    speaker: Mutex<Option<Speaker>>,
    /// Bumped by every `stop()`; sentences queued under an older value are
    /// dropped.
    generation: AtomicU64,
}

impl Shared {
    fn speaker(&self) -> MutexGuard<'_, Option<Speaker>> {
        self.speaker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn cancelled(&self, id: UtteranceId) {
        let _ = self.events.send(SpeechEvent::Cancelled(id));
    }

    /// Open the output device if it is not open yet.
    fn open_speaker(self: &Arc<Self>) -> Result<(), TtsError> {
        let mut speaker = self.speaker();
        if speaker.is_none() {
            let events = self.events.clone();
            let listener = Box::new(move |event: PlaybackEvent| {
                let _ = events.send(match event {
                    PlaybackEvent::Started(tag) => SpeechEvent::Started(UtteranceId(tag)),
                    PlaybackEvent::Finished(tag) => SpeechEvent::Finished(UtteranceId(tag)),
                    PlaybackEvent::Dropped(tag) => SpeechEvent::Cancelled(UtteranceId(tag)),
                });
            });
            *speaker = Some(Speaker::open(listener).map_err(|e| TtsError::Engine(e.to_string()))?);
        }
        Ok(())
    }
}

/// The Kokoro voice. Events arrive on the receiver returned by
/// [`KokoroVoice::start`]; unlike the system voice, no main run loop is
/// needed.
pub struct KokoroVoice {
    commands: mpsc::Sender<Command>,
    shared: Arc<Shared>,
    next_id: AtomicU64,
    voices_dir: std::path::PathBuf,
    /// The pace as f32 bits, read by the engine for each sentence.
    speed: Arc<AtomicU32>,
}

impl KokoroVoice {
    /// Load the model, phonemizer and voice on a new engine thread. Blocks
    /// until they are loaded (about a second), so load errors surface here.
    pub fn start(config: KokoroConfig) -> Result<(Self, UnboundedReceiver<SpeechEvent>), TtsError> {
        let (events, receiver) = unbounded_channel();
        let shared = Arc::new(Shared {
            events,
            speaker: Mutex::new(None),
            generation: AtomicU64::new(0),
        });
        let (commands, inbox) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let engine_shared = Arc::clone(&shared);
        let speed = Arc::new(AtomicU32::new(config.speed.to_bits()));
        let engine_speed = Arc::clone(&speed);
        let voices_dir = config.voices_dir.clone();
        std::thread::Builder::new()
            .name("glance-kokoro".into())
            .spawn(move || {
                let loaded = (|| -> Result<_, TtsError> {
                    let style = load_voice(&config.voices_dir, &config.voice)?;
                    let phonemizer = Misaki::new(Accent::of_voice(config.voice.lead()));
                    let model = KokoroModel::load(&config.onnx, &config.tokenizer, config.threads)?;
                    Ok((model, phonemizer, style))
                })();
                match loaded {
                    Ok((model, phonemizer, style)) => {
                        let _ = ready_tx.send(Ok(()));
                        engine(
                            model,
                            phonemizer,
                            style,
                            &config,
                            &engine_speed,
                            &engine_shared,
                            &inbox,
                        );
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })
            .map_err(|e| TtsError::Engine(format!("could not start the Kokoro thread: {e}")))?;
        ready_rx.recv().map_err(|_| TtsError::EngineGone)??;
        Ok((
            Self {
                commands,
                shared,
                next_id: AtomicU64::new(1),
                voices_dir,
                speed,
            },
            receiver,
        ))
    }
}

impl KokoroVoice {
    /// Use another voice or blend from the next sentence (settings window).
    /// The voice file is read and checked here; an error leaves the old one.
    pub fn set_voice(&self, choice: &VoiceChoice) -> Result<(), TtsError> {
        let style = load_voice(&self.voices_dir, choice)?;
        self.commands
            .send(Command::SetVoice(
                Box::new(style),
                Accent::of_voice(choice.lead()),
            ))
            .map_err(|_| TtsError::EngineGone)
    }

    /// Kokoro pace (0.5 to 2.0) from the next sentence.
    pub fn set_speed(&self, speed: f32) {
        self.speed
            .store(speed.clamp(0.5, 2.0).to_bits(), Ordering::Relaxed);
    }
}

impl TextToSpeech for KokoroVoice {
    fn speak(&self, text: &str) -> Result<UtteranceId, TtsError> {
        let id = UtteranceId(self.next_id.fetch_add(1, Ordering::Relaxed));
        self.commands
            .send(Command::Speak {
                id,
                text: text.to_owned(),
                generation: self.shared.generation.load(Ordering::SeqCst),
            })
            .map_err(|_| TtsError::EngineGone)?;
        Ok(id)
    }

    fn stop(&self) {
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        if let Some(speaker) = self.shared.speaker().as_ref() {
            speaker.stop();
        }
    }

    /// Open the output device now (at key-down), so the first sentence does
    /// not pay for it.
    fn warm_up(&self) {
        let _ = self.commands.send(Command::WarmUp);
    }
}

/// Body of the engine thread. Ends when the [`KokoroVoice`] is dropped.
/// The log line for a sentence that was not spoken: its length and the
/// error's fixed code, never the sentence or the error's text (which can
/// quote it, and the sentence is part of an answer).
fn failure_line(sentence: &str, error: &TtsError) -> String {
    format!(
        "kokoro: a sentence of {} characters was not spoken ({})",
        sentence.chars().count(),
        error.code()
    )
}

#[allow(clippy::too_many_arguments)]
fn engine(
    mut model: KokoroModel,
    mut phonemizer: Misaki,
    mut style: VoiceStyle,
    config: &KokoroConfig,
    speed: &AtomicU32,
    shared: &Arc<Shared>,
    inbox: &mpsc::Receiver<Command>,
) {
    let mut last_active = Instant::now();
    loop {
        let command = match inbox.recv_timeout(Duration::from_secs(1)) {
            Ok(command) => command,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let mut speaker = shared.speaker();
                if let Some(s) = speaker.as_ref() {
                    if s.is_busy() {
                        last_active = Instant::now();
                    } else if last_active.elapsed() >= config.idle_close {
                        *speaker = None;
                    }
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        last_active = Instant::now();
        match command {
            Command::SetVoice(new_style, accent) => {
                style = *new_style;
                if accent != phonemizer.accent() {
                    phonemizer = Misaki::new(accent);
                }
            }
            Command::WarmUp => {
                if let Err(e) = shared.open_speaker() {
                    eprintln!("kokoro: {e}");
                }
            }
            Command::Speak {
                id,
                text,
                generation,
            } => {
                let current = || shared.generation.load(Ordering::SeqCst) == generation;
                if !current() {
                    shared.cancelled(id);
                    continue;
                }
                let samples = phonemizer.phonemize(&text).and_then(|phonemes| {
                    let speed = f32::from_bits(speed.load(Ordering::Relaxed));
                    model.synthesize(&phonemes, &style, speed)
                });
                let volume = config.volume.clamp(0.0, 1.0);
                let played = samples.and_then(|mut samples| {
                    samples.iter_mut().for_each(|s| *s *= volume);
                    // A stop() during synthesis drops this sentence.
                    if !current() || samples.is_empty() {
                        return Ok(false);
                    }
                    shared.open_speaker()?;
                    let speaker = shared.speaker();
                    let Some(speaker) = speaker.as_ref() else {
                        return Ok(false);
                    };
                    speaker
                        .play(id.0, &AudioClip::new(samples, SAMPLE_RATE))
                        .map_err(|e| TtsError::Engine(e.to_string()))?;
                    Ok(true)
                });
                match played {
                    Ok(true) => {}
                    Ok(false) => shared.cancelled(id),
                    Err(e) => {
                        eprintln!("{}", failure_line(&text, &e));
                        shared.cancelled(id);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_sentence_is_logged_without_its_text() {
        let sentence = "The total is 51,900 for Priyanka's marketing budget.";
        for error in [
            TtsError::Phonemizer { chars: 52 },
            // Even an error whose own text quotes the sentence.
            TtsError::Engine(format!("no phonemes matched for '{sentence}'")),
            TtsError::EngineGone,
        ] {
            let line = failure_line(sentence, &error);
            for word in [
                "total",
                "51,900",
                "Priyanka",
                "marketing",
                "budget",
                "phonemes",
            ] {
                assert!(!line.contains(word), "{line}");
            }
            assert!(
                line.contains(&sentence.chars().count().to_string()),
                "{line}"
            );
            assert!(line.contains(error.code()), "{line}");
        }
    }
}
