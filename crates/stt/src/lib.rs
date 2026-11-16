//! Speech-to-text behind the [`SpeechToText`] trait.
//!
//! The implementation is NVIDIA Parakeet TDT (int8 ONNX) through
//! `transcribe-rs` on ONNX Runtime. See `docs/DESIGN.md` §5.

use std::path::Path;

use glance_audio::AudioClip;
use transcribe_rs::onnx::Quantization;
use transcribe_rs::onnx::parakeet::{ParakeetModel, ParakeetParams};

/// Sample rate every STT model here expects.
pub const SAMPLE_RATE: u32 = 16_000;

#[derive(Debug, thiserror::Error)]
pub enum SttError {
    #[error("speech model expects {SAMPLE_RATE} Hz audio, got {0} Hz")]
    SampleRate(u32),
    #[error("could not load the speech model: {0}")]
    Load(String),
    #[error("transcription failed: {0}")]
    Inference(String),
    #[error("could not read audio file: {0}")]
    Audio(String),
}

/// What the user said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    pub text: String,
}

/// A local speech recognizer. Blocking and CPU-heavy: call it from a
/// dedicated thread, not from the async runtime.
pub trait SpeechToText: Send {
    fn transcribe(&mut self, clip: &AudioClip) -> Result<Transcript, SttError>;
}

/// Parakeet TDT 0.6b, int8.
pub struct Parakeet {
    model: ParakeetModel,
}

impl Parakeet {
    /// `dir` must be a directory already verified against `models.toml`
    /// (`ModelStore::verified_dir`).
    pub fn load(dir: &Path) -> Result<Self, SttError> {
        let model = ParakeetModel::load(dir, &Quantization::Int8)
            .map_err(|e| SttError::Load(e.to_string()))?;
        Ok(Self { model })
    }
}

impl SpeechToText for Parakeet {
    fn transcribe(&mut self, clip: &AudioClip) -> Result<Transcript, SttError> {
        if clip.sample_rate() != SAMPLE_RATE {
            return Err(SttError::SampleRate(clip.sample_rate()));
        }
        let result = self
            .model
            .transcribe_with(clip.samples(), &ParakeetParams::default())
            .map_err(|e| SttError::Inference(e.to_string()))?;
        Ok(Transcript {
            text: result.text.trim().to_owned(),
        })
    }
}

/// Read a 16 kHz mono WAV file. Only for benchmarks on synthetic clips: the
/// app itself never reads or writes audio files.
pub fn read_wav(path: &Path) -> Result<AudioClip, SttError> {
    let samples =
        transcribe_rs::audio::read_wav_samples(path).map_err(|e| SttError::Audio(e.to_string()))?;
    Ok(AudioClip::new(samples, SAMPLE_RATE))
}
