//! Speaker output on cpal: queued mono clips played back to back.
//!
//! The audio callback runs on CoreAudio's high-priority thread. It only
//! `try_lock`s the queue (on contention it plays one buffer of silence
//! rather than wait) and reports when a clip starts and finishes. Clips are
//! resampled to the device rate before they are queued, so the callback
//! never resamples.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};

use crate::{AudioClip, AudioError, resample};

/// What playback reports, tagged with the caller's id for the clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackEvent {
    /// The clip's first sample went to the device.
    Started(u64),
    /// Its last sample went to the device.
    Finished(u64),
    /// It was dropped by [`AudioOutput::stop`] before finishing.
    Dropped(u64),
}

/// Called with every [`PlaybackEvent`], on the audio thread: keep it short
/// and non-blocking (sending on a channel is fine).
pub type PlaybackListener = Box<dyn Fn(PlaybackEvent) + Send + Sync>;

/// A speaker that plays queued clips in order.
pub trait AudioOutput: Send {
    /// Queue `clip` after everything already queued. Resampled to the
    /// device rate here, on the caller's thread.
    fn play(&self, tag: u64, clip: &AudioClip) -> Result<(), AudioError>;

    /// Stop at once and drop everything queued.
    fn stop(&self);

    /// Whether anything is queued or playing.
    fn is_busy(&self) -> bool;
}

struct Segment {
    tag: u64,
    samples: Vec<f32>,
    position: usize,
}

struct Shared {
    queue: Mutex<VecDeque<Segment>>,
    listener: PlaybackListener,
}

impl Shared {
    fn queue(&self) -> MutexGuard<'_, VecDeque<Segment>> {
        self.queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The default output device, as a running cpal stream (silence when
/// nothing is queued). Dropping it closes the device.
pub struct Speaker {
    _stream: cpal::Stream,
    shared: Arc<Shared>,
    sample_rate: u32,
}

impl Speaker {
    /// Open the default output device and start the stream.
    pub fn open(listener: PlaybackListener) -> Result<Self, AudioError> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or(AudioError::NoOutputDevice)?;
        let supported = device
            .default_output_config()
            .map_err(|e| AudioError::Device(e.to_string()))?;
        if supported.sample_format() != SampleFormat::F32 {
            return Err(AudioError::UnsupportedFormat(format!(
                "{:?} output",
                supported.sample_format()
            )));
        }
        let config: StreamConfig = supported.config();
        let channels = usize::from(config.channels).max(1);
        let sample_rate = config.sample_rate;
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            listener,
        });
        let callback_shared = Arc::clone(&shared);
        let mut events = Vec::with_capacity(8);
        let stream = device
            .build_output_stream(
                config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    events.clear();
                    match callback_shared.queue.try_lock() {
                        Ok(mut queue) => fill(&mut queue, data, channels, &mut events),
                        Err(_) => data.fill(0.0),
                    }
                    for &event in &events {
                        (callback_shared.listener)(event);
                    }
                },
                |e: cpal::Error| eprintln!("speaker stream error: {e}"),
                None,
            )
            .map_err(|e| AudioError::Device(e.to_string()))?;
        stream
            .play()
            .map_err(|e| AudioError::Device(e.to_string()))?;
        Ok(Self {
            _stream: stream,
            shared,
            sample_rate,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

impl AudioOutput for Speaker {
    fn play(&self, tag: u64, clip: &AudioClip) -> Result<(), AudioError> {
        let samples = resample(clip, self.sample_rate)?.samples().to_vec();
        self.shared.queue().push_back(Segment {
            tag,
            samples,
            position: 0,
        });
        Ok(())
    }

    fn stop(&self) {
        let dropped: Vec<u64> = self.shared.queue().drain(..).map(|s| s.tag).collect();
        for tag in dropped {
            (self.shared.listener)(PlaybackEvent::Dropped(tag));
        }
    }

    fn is_busy(&self) -> bool {
        !self.shared.queue().is_empty()
    }
}

/// Audio callback body: copy queued mono samples into the interleaved
/// output buffer, silence after the queue runs dry.
fn fill(
    queue: &mut VecDeque<Segment>,
    out: &mut [f32],
    channels: usize,
    events: &mut Vec<PlaybackEvent>,
) {
    for frame in out.chunks_exact_mut(channels) {
        let sample = loop {
            let Some(segment) = queue.front_mut() else {
                break 0.0;
            };
            if segment.position == 0 {
                events.push(PlaybackEvent::Started(segment.tag));
            }
            if let Some(&s) = segment.samples.get(segment.position) {
                segment.position += 1;
                break s;
            }
            events.push(PlaybackEvent::Finished(segment.tag));
            queue.pop_front();
        };
        frame.fill(sample);
    }
    // Report a clip that ended exactly at the end of this buffer now, not
    // one buffer later.
    if let Some(segment) = queue.front()
        && segment.position == segment.samples.len()
    {
        events.push(PlaybackEvent::Finished(segment.tag));
        queue.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(tag: u64, samples: &[f32]) -> Segment {
        Segment {
            tag,
            samples: samples.to_vec(),
            position: 0,
        }
    }

    #[test]
    fn clips_play_back_to_back_on_every_channel() {
        let mut queue = VecDeque::from([segment(1, &[0.1, 0.2]), segment(2, &[0.3])]);
        let mut out = [9.0f32; 8];
        let mut events = Vec::new();
        fill(&mut queue, &mut out, 2, &mut events);
        assert_eq!(out, [0.1, 0.1, 0.2, 0.2, 0.3, 0.3, 0.0, 0.0]);
        use PlaybackEvent::*;
        assert_eq!(events, [Started(1), Finished(1), Started(2), Finished(2)]);
        assert!(queue.is_empty());
    }

    #[test]
    fn a_clip_spanning_buffers_starts_once_and_finishes_once() {
        let mut queue = VecDeque::from([segment(7, &[0.5; 5])]);
        let mut events = Vec::new();
        let mut out = [0.0f32; 3];
        fill(&mut queue, &mut out, 1, &mut events);
        assert_eq!(events, [PlaybackEvent::Started(7)]);
        events.clear();
        fill(&mut queue, &mut out, 1, &mut events);
        assert_eq!(out, [0.5, 0.5, 0.0]);
        assert_eq!(events, [PlaybackEvent::Finished(7)]);
    }

    #[test]
    fn a_clip_ending_on_the_buffer_edge_is_reported_in_that_buffer() {
        let mut queue = VecDeque::from([segment(3, &[0.5; 4])]);
        let mut events = Vec::new();
        fill(&mut queue, &mut [0.0f32; 4], 1, &mut events);
        assert_eq!(
            events,
            [PlaybackEvent::Started(3), PlaybackEvent::Finished(3)]
        );
        assert!(queue.is_empty());
    }
}
