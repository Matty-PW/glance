//! `glance ui-snapshots`: glance's own windows drawn to PNGs (overlay states,
//! the screenshot indicator's looks, settings and setup pages, light and dark,
//! fixed demo text), to review layout.
//! Development only. It never captures the screen.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use glance_orchestrator::settings::{
    Applied, DownloadIssue, DownloadProgress, ModelRow, ModelState, ModelsBackend, PartialRow,
    SettingsBackend, SettingsChange, SettingsChoices, SettingsValues,
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
    written.extend(setup(&args.out)?);
    for path in written {
        println!("{}", path.display());
    }
    Ok(())
}

/// Every setup page and its notable states, on the setup demo's data.
fn setup(out: &std::path::Path) -> Result<Vec<PathBuf>> {
    use glance_macos::signing::Signing;
    use glance_macos::snapshot::SetupScene;
    use glance_orchestrator::setup::{Access, Step};
    let runtime = tokio::runtime::Builder::new_current_thread().build()?;
    let demo = super::app::setup_demo::DemoSetup::new(runtime.handle().clone());
    let scene = |name, step, screen, mic, signing, advanced| SetupScene {
        name,
        step,
        screen,
        mic,
        signing,
        advanced,
    };
    use Access::{Denied, Granted, NeedsRestart, NotAsked};
    let ok = Signing::Stable;
    let scenes = [
        scene("welcome", Step::Welcome, NotAsked, NotAsked, ok, false),
        scene("your-mac", Step::YourMac, NotAsked, NotAsked, ok, false),
        scene(
            "your-mac-advanced",
            Step::YourMac,
            NotAsked,
            NotAsked,
            ok,
            true,
        ),
        scene("your-mac-8gb", Step::YourMac, NotAsked, NotAsked, ok, true),
        scene(
            "permissions",
            Step::Permissions,
            NotAsked,
            NotAsked,
            ok,
            false,
        ),
        scene(
            "permissions-denied-adhoc",
            Step::Permissions,
            Denied,
            Denied,
            Signing::AdHoc,
            false,
        ),
        scene(
            "permissions-restart",
            Step::Permissions,
            NeedsRestart,
            Granted,
            Signing::AdHoc,
            false,
        ),
        scene(
            "permissions-unbundled",
            Step::Permissions,
            Granted,
            Granted,
            Signing::NotBundled,
            false,
        ),
        scene("download", Step::Download, Granted, Granted, ok, false),
        scene(
            "download-running",
            Step::Download,
            Granted,
            Granted,
            ok,
            false,
        ),
        scene(
            "download-failed",
            Step::Download,
            Granted,
            Granted,
            ok,
            false,
        ),
        scene("download-done", Step::Download, Granted, Granted, ok, false),
        scene("try-it", Step::TryIt, Granted, Granted, ok, false),
        scene("try-it-listening", Step::TryIt, Granted, Granted, ok, false),
        scene("try-it-works", Step::TryIt, Granted, Granted, ok, false),
        scene("try-it-failed", Step::TryIt, Granted, Granted, ok, false),
        scene("done", Step::Done, Granted, Granted, ok, false),
        scene("all-set", Step::AllSet, Granted, Granted, ok, false),
    ];
    let backend = Arc::clone(&demo) as Arc<dyn glance_orchestrator::setup::SetupBackend>;
    let mut written =
        glance_macos::snapshot::setup(out, backend, &scenes, &|name| demo.set_scene(name))
            .map_err(anyhow::Error::msg)?;
    written.push(glance_macos::snapshot::sample_chart(out).map_err(anyhow::Error::msg)?);
    Ok(written)
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
                ModelState::Paused {
                    done: 41_000_000,
                    total: 86_000_000,
                },
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

    fn download_many(&self, _ids: &[String]) -> Result<(), DownloadIssue> {
        Ok(())
    }

    fn progress(&self) -> Option<DownloadProgress> {
        None
    }

    fn pause(&self) {}

    fn resume(&self, _id: Option<&str>) -> Result<(), DownloadIssue> {
        Ok(())
    }

    fn partials(&self) -> Vec<PartialRow> {
        vec![
            PartialRow {
                name: "a.part".into(),
                belongs_to: Some((
                    "kokoro-82m-v1.0-q8f16".into(),
                    "onnx/model_q8f16.onnx".into(),
                )),
                bytes: 41_000_000,
            },
            PartialRow {
                name: "b.part".into(),
                belongs_to: None,
                bytes: 1_200_000_000,
            },
        ]
    }

    fn clear_partials(&self) -> u64 {
        0
    }

    fn verify(&self, _id: &str) -> Result<(), String> {
        Ok(())
    }

    fn cancel(&self) {}

    fn delete(&self, _id: &str) -> Result<u64, String> {
        Ok(0)
    }
}
