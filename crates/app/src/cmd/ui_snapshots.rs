//! `glance ui-snapshots`: glance's own windows drawn to PNGs (overlay states,
//! the screenshot indicator's looks and settings pages, light and dark,
//! fixed demo text), to review layout.
//! Development only. It never captures the screen.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use glance_orchestrator::settings::{
    Applied, ModelRow, ModelState, ModelsBackend, SettingsBackend, SettingsChange, SettingsChoices,
    SettingsValues,
};

#[derive(clap::Args)]
pub struct Args {
    /// Where the PNGs go (keep it in the gitignored eval-local/).
    #[arg(long, default_value = "eval-local/ui")]
    out: PathBuf,
}

pub fn run(args: Args) -> Result<()> {
    eprintln!(
        "glance: drawing glance's own windows with demo text into {} (no screen capture)",
        args.out.display()
    );
    let mut written = glance_macos::snapshot::overlay(&args.out).map_err(anyhow::Error::msg)?;
    written.extend(glance_macos::snapshot::glow(&args.out).map_err(anyhow::Error::msg)?);
    written.extend(
        glance_macos::snapshot::settings(&args.out, Arc::new(DemoSettings), Arc::new(DemoModels))
            .map_err(anyhow::Error::msg)?,
    );
    for path in written {
        println!("{}", path.display());
    }
    Ok(())
}

/// Demo settings with the longest names and labels glance can show, so the
/// snapshots show the worst case.
struct DemoSettings;

impl SettingsBackend for DemoSettings {
    fn current(&self) -> (SettingsValues, SettingsChoices) {
        let values = SettingsValues {
            deep_model: Some("qwen3.8-27b-q4km-with-a-very-long-community-conversion-name".into()),
            personality: "custom".into(),
            personality_custom: "Calm and patient, like a good teacher who explains every step."
                .into(),
            ..self.defaults()
        };
        let choices = SettingsChoices {
            kokoro_voices: ["af_heart", "af_bella", "bf_emma", "bm_george"]
                .map(String::from)
                .to_vec(),
            system_voices: vec![(
                "com.apple.voice.premium.en-GB.Serena".into(),
                "Serena (Premium) (en-GB)".into(),
            )],
            models: vec![
                "qwen3-vl-4b-instruct-q4km".into(),
                "qwen3.8-27b-q4km-with-a-very-long-community-conversion-name".into(),
            ],
        };
        (values, choices)
    }

    fn defaults(&self) -> SettingsValues {
        let d = crate::config::Config::default();
        SettingsValues {
            engine: "kokoro".into(),
            kokoro_voice: d.speech.voice,
            system_voice: None,
            speed: d.speech.speed,
            math_as_words: d.speech.math_as_words,
            length: d.models.response_length(),
            personality: d.models.personality.clone(),
            personality_custom: d.models.personality_custom.clone(),
            fast_model: d.models.fast.clone(),
            deep_model: d.models.deep.clone(),
            auto_escalate: d.models.auto_escalate,
            deep_effort: d.models.deep_effort.clone(),
            deep_thinking_budget: d.models.thinking_budget(),
            deep_idle_mins: d.models.deep_idle_secs / 60,
            hotkeys: [d.hotkeys.talk, d.hotkeys.look, d.hotkeys.think],
            accent: d.appearance.accent,
            screenshot_indicator: d.appearance.screenshot_indicator,
        }
    }

    fn apply(&self, _change: SettingsChange) -> Result<Applied, String> {
        Ok(Applied::Live)
    }
}

struct DemoModels;

impl ModelsBackend for DemoModels {
    fn rows(&self) -> Vec<ModelRow> {
        let row =
            |id: &str, role: &str, gb: f64, state: ModelState, in_use: Option<&str>| ModelRow {
                id: id.into(),
                role: role.into(),
                license: "Apache-2.0".into(),
                repo: "example-org/example-repository-name-GGUF".into(),
                revision: "0123456789abcdef0123456789abcdef01234567".into(),
                bytes: (gb * 1e9) as u64,
                bytes_missing: if matches!(state, ModelState::NotDownloaded) {
                    (gb * 1e9) as u64
                } else {
                    0
                },
                state,
                in_use: in_use.map(String::from),
            };
        vec![
            row(
                "qwen3-vl-4b-instruct-q4km",
                "vision",
                3.3,
                ModelState::Verified,
                Some("FAST model"),
            ),
            row(
                "qwen3.8-27b-q4km-with-a-very-long-community-conversion-name",
                "vision",
                17.4,
                ModelState::Downloading {
                    done: 6_100_000_000,
                    total: 17_400_000_000,
                },
                None,
            ),
            row(
                "parakeet-tdt-0.6b-v3-int8",
                "speech-to-text",
                0.67,
                ModelState::Downloaded,
                Some("speech recognition"),
            ),
            row(
                "kokoro-82m-v1.0-q8f16",
                "voice",
                0.09,
                ModelState::NotDownloaded,
                None,
            ),
            row(
                "lfm2.5-vl-3b-q4km",
                "vision",
                2.1,
                ModelState::Failed("SHA-256 does not match its pin".into()),
                None,
            ),
        ]
    }

    fn free_bytes(&self) -> Option<u64> {
        Some(212_000_000_000)
    }

    fn download(&self, _id: &str) -> Result<(), String> {
        Ok(())
    }

    fn verify(&self, _id: &str) -> Result<(), String> {
        Ok(())
    }

    fn cancel(&self) {}

    fn delete(&self, _id: &str) -> Result<u64, String> {
        Ok(0)
    }
}
