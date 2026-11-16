//! Microphone input and speaker output behind the `AudioInput` and `AudioOutput`
//! traits (cpal), plus resampling and the ring buffers used by audio callbacks.
//!
//! First used in M0. Recorded audio is held in memory only.
//! See `docs/DESIGN.md` §2–4.
