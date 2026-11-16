//! Text-to-speech behind the `TextToSpeech` trait, plus `SentenceChunker`.
//!
//! The macOS system voice is the first implementation (M1); Kokoro follows if
//! the phonemizer spike passes. See `docs/DESIGN.md` §6.
