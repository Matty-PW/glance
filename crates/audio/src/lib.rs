//! Microphone input behind the [`AudioInput`] trait, plus resampling.
//!
//! Recorded audio lives in [`AudioClip`], an in-memory type with no
//! `Serialize`, no `Display`, and a `Debug` that prints only duration and
//! rate, so samples cannot reach a log or a file by accident.
//!
//! Speaker output (`AudioOutput`) arrives with TTS in M1.
//! See `docs/DESIGN.md` §2–4.

mod mic;
#[cfg(target_os = "macos")]
pub mod permission;
mod resample;

use std::fmt;
use std::time::Duration;

pub use mic::Microphone;
pub use resample::resample;

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("no microphone found")]
    NoInputDevice,
    #[error("microphone uses an unsupported sample format ({0})")]
    UnsupportedFormat(String),
    #[error("audio device error: {0}")]
    Device(String),
    #[error("recording was not started")]
    NotRecording,
    #[error("resampling failed: {0}")]
    Resample(String),
}

/// Mono audio samples in memory.
#[derive(Clone)]
pub struct AudioClip {
    samples: Vec<f32>,
    sample_rate: u32,
}

impl AudioClip {
    pub fn new(samples: Vec<f32>, sample_rate: u32) -> Self {
        Self {
            samples,
            sample_rate,
        }
    }

    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.samples.len() as f64 / f64::from(self.sample_rate.max(1)))
    }

    /// True when every sample is exactly zero, which is what macOS delivers
    /// when microphone permission has not been granted.
    pub fn is_silent(&self) -> bool {
        self.samples.iter().all(|&s| s == 0.0)
    }
}

impl fmt::Debug for AudioClip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "AudioClip({:.2} s, {} Hz)",
            self.duration().as_secs_f64(),
            self.sample_rate
        )
    }
}

/// What a finished recording yields.
#[derive(Debug)]
pub struct Recording {
    pub clip: AudioClip,
    /// Delay between asking the device to start and the first samples
    /// arriving. Speech during this gap is lost, so it is logged.
    pub start_latency: Option<Duration>,
}

/// A push-to-talk microphone: samples are captured only between `start` and
/// `stop`, and no stream exists outside that window.
pub trait AudioInput: Send {
    fn start(&mut self) -> Result<(), AudioError>;
    fn stop(&mut self) -> Result<Recording, AudioError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_samples() {
        let clip = AudioClip::new(vec![0.123_456; 16_000], 16_000);
        assert_eq!(format!("{clip:?}"), "AudioClip(1.00 s, 16000 Hz)");
    }

    #[test]
    fn silence_detection() {
        assert!(AudioClip::new(vec![0.0; 10], 16_000).is_silent());
        assert!(!AudioClip::new(vec![0.0, 0.001], 16_000).is_silent());
    }
}
