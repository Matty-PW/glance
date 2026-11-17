//! One-shot resampling of a whole clip (microphone rate to the STT rate).

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};

use crate::{AudioClip, AudioError};

/// Resample `clip` to `target_rate`. Returns a copy when the rate already
/// matches.
pub fn resample(clip: &AudioClip, target_rate: u32) -> Result<AudioClip, AudioError> {
    // NaN or infinity would make the FFT resampler panic; play silence there.
    let cleaned: Vec<f32>;
    let input = if clip.samples().iter().all(|s| s.is_finite()) {
        clip.samples()
    } else {
        cleaned = clip
            .samples()
            .iter()
            .map(|&s| if s.is_finite() { s } else { 0.0 })
            .collect();
        &cleaned
    };
    if clip.sample_rate() == target_rate || input.is_empty() {
        return Ok(AudioClip::new(input.to_vec(), target_rate));
    }
    let err = |e: &dyn std::fmt::Display| AudioError::Resample(e.to_string());

    let mut resampler = Fft::<f32>::new(
        clip.sample_rate() as usize,
        target_rate as usize,
        1024,
        1,
        FixedSync::Both,
    )
    .map_err(|e| err(&e))?;
    let needed = resampler.process_all_needed_output_len(input.len());
    let mut output = vec![0.0f32; needed];

    let source = InterleavedSlice::new(input, 1, input.len()).map_err(|e| err(&e))?;
    let mut sink = InterleavedSlice::new_mut(&mut output, 1, needed).map_err(|e| err(&e))?;
    let (_consumed, produced) = resampler
        .process_all_into_buffer(&source, &mut sink, input.len(), None)
        .map_err(|e| err(&e))?;

    output.truncate(produced);
    Ok(AudioClip::new(output, target_rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, hz: f32, seconds: f32) -> AudioClip {
        let n = (rate as f32 * seconds) as usize;
        let samples = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin() * 0.5)
            .collect();
        AudioClip::new(samples, rate)
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    #[test]
    fn non_finite_samples_become_silence_instead_of_a_panic() {
        let mut clip = sine(24_000, 440.0, 0.5).samples().to_vec();
        clip[100] = f32::NAN;
        clip[200] = f32::INFINITY;
        let out = resample(&AudioClip::new(clip, 24_000), 48_000).unwrap();
        assert!(out.samples().iter().all(|s| s.is_finite()));
        assert_eq!(out.sample_rate(), 48_000);
    }

    #[test]
    fn keeps_duration_and_level() {
        for rate in [48_000, 44_100] {
            let clip = sine(rate, 440.0, 1.0);
            let out = resample(&clip, 16_000).unwrap();
            assert_eq!(out.sample_rate(), 16_000);
            let len = out.samples().len() as i64;
            assert!((len - 16_000).abs() < 200, "{rate}: {len} samples");
            // A 440 Hz tone is well inside the passband: level must survive.
            let middle = &out.samples()[2_000..14_000];
            assert!(
                (rms(middle) - 0.3536).abs() < 0.02,
                "{rate}: rms {}",
                rms(middle)
            );
        }
    }

    #[test]
    fn same_rate_is_a_copy() {
        let clip = sine(16_000, 440.0, 0.1);
        let out = resample(&clip, 16_000).unwrap();
        assert_eq!(out.samples(), clip.samples());
    }
}
