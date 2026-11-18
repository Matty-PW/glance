pub mod app;
pub mod bench;
pub mod cache_check;
pub mod capture_check;
pub mod deep_bench;
pub mod fast_bench;
pub mod mic_check;
pub mod models;
pub mod say;
pub mod serve;
pub mod server_check;
pub mod speech;
pub mod spike;
pub mod stt_bench;
pub mod tts_bench;

use std::time::Instant;

use anyhow::{Context, Result};
use glance_models::{Manifest, Model, ModelStore};
use glance_tts::SpeechEvent;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::watch;

/// System prompt for spoken replies, shared with the app.
pub use glance_orchestrator::SPOKEN_SYSTEM_PROMPT;

pub fn manifest() -> Result<Manifest> {
    Manifest::builtin().context("the built-in models.toml is invalid")
}

pub fn find_model<'a>(manifest: &'a Manifest, id: &str) -> Result<&'a Model> {
    manifest.model(id).with_context(|| {
        let known: Vec<&str> = manifest.models().iter().map(|m| m.id.as_str()).collect();
        format!(
            "no model `{id}` in models.toml (known: {})",
            known.join(", ")
        )
    })
}

pub fn store() -> Result<ModelStore> {
    Ok(ModelStore::default_location()?)
}

/// The Kokoro precisions in `models.toml`, by short name.
pub const KOKORO_MODELS: &[(&str, &str)] = &[
    ("fp32", "kokoro-82m-v1.0-fp32"),
    ("fp16", "kokoro-82m-v1.0-fp16"),
    ("q8f16", "kokoro-82m-v1.0-q8f16"),
];
pub const KOKORO_VOICES: &str = "kokoro-82m-v1.0-voices-en";

/// Verified paths of a Kokoro model (`fp32`, `fp16`, `q8f16` or a full id):
/// the ONNX file and tokenizer.json. Hashes every file first.
pub fn kokoro_model_paths(name: &str) -> Result<(std::path::PathBuf, std::path::PathBuf)> {
    let id = KOKORO_MODELS
        .iter()
        .find(|(short, _)| *short == name)
        .map_or(name, |(_, id)| id);
    let manifest = manifest()?;
    let model = find_model(&manifest, id)?;
    let dir = store()?
        .verified_dir(model)
        .with_context(|| format!("run `glance models pull {id}` first"))?;
    let onnx = model
        .files
        .iter()
        .find(|f| f.kind == glance_models::Kind::Weights)
        .with_context(|| format!("{id} has no weights file"))?;
    Ok((dir.join(&onnx.path), dir.join("tokenizer.json")))
}

/// The verified Kokoro voices directory.
pub fn kokoro_voices_dir() -> Result<std::path::PathBuf> {
    let manifest = manifest()?;
    let model = find_model(&manifest, KOKORO_VOICES)?;
    store()?
        .verified_dir(model)
        .with_context(|| format!("run `glance models pull {KOKORO_VOICES}` first"))
}

/// Milliseconds with one decimal, for timing reports.
pub fn ms(duration: std::time::Duration) -> f64 {
    (duration.as_secs_f64() * 10_000.0).round() / 10.0
}

/// What has been heard back from the speech engine so far.
#[derive(Debug, Clone, Copy, Default)]
pub struct SpeechProgress {
    /// When the first sentence started to sound.
    pub first_audio: Option<Instant>,
    /// Sentences that are over, spoken or cancelled.
    pub ended: u64,
    pub cancelled: u64,
}

/// Timestamp speech events as they arrive, on a helper thread, and publish
/// the running totals.
pub fn track_speech(mut events: UnboundedReceiver<SpeechEvent>) -> watch::Receiver<SpeechProgress> {
    let (tx, rx) = watch::channel(SpeechProgress::default());
    std::thread::spawn(move || {
        while let Some(event) = events.blocking_recv() {
            let now = Instant::now();
            tx.send_modify(|progress| match event {
                SpeechEvent::Started(_) => {
                    progress.first_audio.get_or_insert(now);
                }
                SpeechEvent::Finished(_) => progress.ended += 1,
                SpeechEvent::Cancelled(_) => {
                    progress.ended += 1;
                    progress.cancelled += 1;
                }
            });
        }
    });
    rx
}
