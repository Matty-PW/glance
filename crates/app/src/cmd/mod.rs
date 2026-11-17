pub mod bench;
pub mod cache_check;
pub mod capture_check;
pub mod mic_check;
pub mod models;
pub mod say;
pub mod serve;
pub mod spike;
pub mod stt_bench;

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
