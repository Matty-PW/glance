//! cpal-backed microphone.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};

use crate::{AudioClip, AudioError, AudioInput, Recording};

/// State shared with the audio callback, which runs on CoreAudio's
/// high-priority thread. The callback never allocates (the buffer is
/// pre-sized) and never blocks (`try_lock`; the only other locker runs after
/// the stream has stopped).
struct Shared {
    samples: Mutex<Vec<f32>>,
    /// Nanoseconds from `start()` to the first callback, plus one. 0 = none yet.
    first_callback: AtomicU64,
    started: Instant,
}

struct Active {
    stream: cpal::Stream,
    shared: Arc<Shared>,
    sample_rate: u32,
}

/// The default input device, opened only while recording.
pub struct Microphone {
    max_seconds: u32,
    active: Option<Active>,
}

impl Microphone {
    /// `max_seconds` caps a single recording; samples beyond it are dropped.
    pub fn new(max_seconds: u32) -> Self {
        Self {
            max_seconds,
            active: None,
        }
    }
}

impl AudioInput for Microphone {
    fn start(&mut self) -> Result<(), AudioError> {
        let device = cpal::default_host()
            .default_input_device()
            .ok_or(AudioError::NoInputDevice)?;
        let supported = device
            .default_input_config()
            .map_err(|e| AudioError::Device(e.to_string()))?;
        let format = supported.sample_format();
        let config: StreamConfig = supported.config();
        let channels = usize::from(config.channels).max(1);
        let sample_rate = config.sample_rate;

        let capacity = sample_rate as usize * self.max_seconds as usize;
        let shared = Arc::new(Shared {
            samples: Mutex::new(Vec::with_capacity(capacity)),
            first_callback: AtomicU64::new(0),
            started: Instant::now(),
        });

        let on_error = |e: cpal::Error| eprintln!("microphone stream error: {e}");
        let stream = match format {
            SampleFormat::F32 => {
                let shared = Arc::clone(&shared);
                device.build_input_stream(
                    config,
                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                        push(&shared, data, channels, |s| s)
                    },
                    on_error,
                    None,
                )
            }
            SampleFormat::I16 => {
                let shared = Arc::clone(&shared);
                device.build_input_stream(
                    config,
                    move |data: &[i16], _: &cpal::InputCallbackInfo| {
                        push(&shared, data, channels, |s| f32::from(s) / 32_768.0)
                    },
                    on_error,
                    None,
                )
            }
            SampleFormat::I32 => {
                let shared = Arc::clone(&shared);
                device.build_input_stream(
                    config,
                    move |data: &[i32], _: &cpal::InputCallbackInfo| {
                        push(&shared, data, channels, |s| s as f32 / 2_147_483_648.0)
                    },
                    on_error,
                    None,
                )
            }
            other => return Err(AudioError::UnsupportedFormat(format!("{other:?}"))),
        }
        .map_err(|e| AudioError::Device(e.to_string()))?;

        stream
            .play()
            .map_err(|e| AudioError::Device(e.to_string()))?;
        self.active = Some(Active {
            stream,
            shared,
            sample_rate,
        });
        Ok(())
    }

    fn stop(&mut self) -> Result<Recording, AudioError> {
        let Active {
            stream,
            shared,
            sample_rate,
        } = self.active.take().ok_or(AudioError::NotRecording)?;
        let _ = stream.pause();
        drop(stream); // the device is released here; no stream outlives the key press

        let samples = std::mem::take(
            &mut *shared
                .samples
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
        let start_latency = match shared.first_callback.load(Ordering::Relaxed) {
            0 => None,
            nanos => Some(Duration::from_nanos(nanos - 1)),
        };
        Ok(Recording {
            clip: AudioClip::new(samples, sample_rate),
            start_latency,
        })
    }
}

/// Audio callback body: downmix interleaved frames to mono and append.
fn push<T: Copy>(shared: &Shared, data: &[T], channels: usize, to_f32: impl Fn(T) -> f32) {
    if shared.first_callback.load(Ordering::Relaxed) == 0 {
        let nanos = shared.started.elapsed().as_nanos() as u64 + 1;
        let _ =
            shared
                .first_callback
                .compare_exchange(0, nanos, Ordering::Relaxed, Ordering::Relaxed);
    }
    if let Ok(mut samples) = shared.samples.try_lock() {
        downmix_into(&mut samples, data, channels, to_f32);
    }
}

/// Append the mono mix of `data` to `out`, never growing past its capacity.
fn downmix_into<T: Copy>(
    out: &mut Vec<f32>,
    data: &[T],
    channels: usize,
    to_f32: impl Fn(T) -> f32,
) {
    let room = out.capacity() - out.len();
    for frame in data.chunks_exact(channels).take(room) {
        let sum: f32 = frame.iter().map(|&s| to_f32(s)).sum();
        out.push(sum / channels as f32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_averages_channels() {
        let mut out = Vec::with_capacity(8);
        downmix_into(&mut out, &[1.0f32, 0.0, 0.5, 0.5, -1.0, 1.0], 2, |s| s);
        assert_eq!(out, [0.5, 0.5, 0.0]);
    }

    #[test]
    fn downmix_converts_integer_samples() {
        let mut out = Vec::with_capacity(4);
        downmix_into(&mut out, &[16_384i16, -16_384], 1, |s| {
            f32::from(s) / 32_768.0
        });
        assert_eq!(out, [0.5, -0.5]);
    }

    #[test]
    fn downmix_never_grows_the_buffer() {
        let mut out = Vec::with_capacity(2);
        let capacity = out.capacity();
        downmix_into(&mut out, &[0.1f32; 100], 1, |s| s);
        assert_eq!(out.len(), capacity);
        assert_eq!(out.capacity(), capacity);
    }

    #[test]
    fn stop_without_start_is_an_error() {
        let mut mic = Microphone::new(1);
        assert!(matches!(mic.stop(), Err(AudioError::NotRecording)));
    }
}
