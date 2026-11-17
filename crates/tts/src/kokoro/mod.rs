//! Kokoro-82M v1.0 (`docs/DESIGN.md` §6.3): phonemizer, voice styles and the
//! ONNX model. The playback engine that implements
//! [`crate::TextToSpeech`] is in [`engine`].

pub mod engine;
pub mod model;
pub mod phonemizer;
pub mod voice;

pub use engine::{KokoroConfig, KokoroVoice, load_voice};
pub use model::{KokoroModel, MAX_TOKENS, SAMPLE_RATE};
pub use phonemizer::{Accent, Misaki, Phonemizer};
pub use voice::{VoiceChoice, VoiceStyle};
