//! Text-to-speech behind the [`TextToSpeech`] trait, plus [`SentenceChunker`],
//! which turns streamed model output into speakable sentences.
//!
//! Engines: Kokoro-82M on ONNX Runtime (here), and the macOS system voice
//! (in the platform crate, `glance-macos`). See `docs/DESIGN.md` §6.

mod chunker;
pub mod kokoro;
mod speakable;

pub use chunker::{ChunkerConfig, SentenceChunker};
pub use speakable::speakable;

/// Identifies one queued sentence in [`SpeechEvent`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UtteranceId(pub u64);

/// What a speech engine reports back. Events arrive in order on the channel
/// handed out when the engine is created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeechEvent {
    /// Audio for this sentence has started. The first `Started` after a
    /// key-up is the time-to-first-audio moment.
    Started(UtteranceId),
    /// The sentence was spoken to the end.
    Finished(UtteranceId),
    /// The sentence was cut off or dropped by [`TextToSpeech::stop`].
    Cancelled(UtteranceId),
}

#[derive(Debug, thiserror::Error)]
pub enum TtsError {
    #[error("the speech engine is no longer running")]
    EngineGone,
    #[error("speech engine error: {0}")]
    Engine(String),
    /// A sentence could not be turned into phonemes. Only its length is
    /// kept: the text is part of an answer (screen-derived), and errors end
    /// up in logs.
    #[error("the phonemizer could not read a sentence of {chars} characters")]
    Phonemizer { chars: usize },
}

impl TtsError {
    /// A fixed code for log lines, which must never carry sentence text.
    pub fn code(&self) -> &'static str {
        match self {
            TtsError::EngineGone => "engine_gone",
            TtsError::Engine(_) => "engine",
            TtsError::Phonemizer { .. } => "phonemizer",
        }
    }
}

/// A speech engine that owns its own playback. Sentences are queued and
/// spoken in order; nothing here blocks.
///
/// The trait deliberately does not return audio, so an implementation may
/// synthesize and play samples itself (Kokoro) or hand the text to the
/// operating system (the system voice).
pub trait TextToSpeech: Send + Sync {
    /// Queue one sentence. Returns at once with the id used in events.
    fn speak(&self, text: &str) -> Result<UtteranceId, TtsError>;

    /// Stop speaking immediately and drop everything queued.
    fn stop(&self);

    /// A reply is coming soon (the user pressed the hotkey): get ready, for
    /// example by opening the output device. Optional.
    fn warm_up(&self) {}
}
