//! What the settings window shows and changes, as plain data, and the
//! [`SettingsBackend`] the app implements. The window itself (native, in the
//! platform crate) sees only these types; the config file and the running
//! pipeline stay on the app's side.
//!
//! Scope (maintainer, 2026-11-18): voice, answer length, personality,
//! models, think-harder options and hotkeys. Everything else is edited in
//! `config.toml`, which stays the source of truth.

use crate::ResponseLength;

/// The current values.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsValues {
    /// "kokoro" or "system".
    pub engine: String,
    /// Kokoro voice id (or blend) and the system voice (identifier or
    /// language; `None` for the system default).
    pub kokoro_voice: String,
    pub system_voice: Option<String>,
    /// Kokoro pace, 0.5 to 2.0.
    pub speed: f32,
    /// Say maths symbols as words.
    pub math_as_words: bool,
    pub length: ResponseLength,
    /// Preset name (see [`crate::personality::PRESETS`]) and the custom
    /// text, kept even while another preset is chosen.
    pub personality: String,
    pub personality_custom: String,
    pub fast_model: String,
    pub deep_model: Option<String>,
    pub auto_escalate: bool,
    /// "off", "low", "medium" or "xhigh".
    pub deep_effort: String,
    pub deep_thinking_budget: Option<u32>,
    pub deep_idle_mins: u64,
    pub hotkeys: [String; 3],
    /// "glance" (glance's accent colour) or "system" (the macOS accent).
    pub accent: String,
}

/// What can be picked.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SettingsChoices {
    pub kokoro_voices: Vec<String>,
    /// (identifier, label) of the English system voices.
    pub system_voices: Vec<(String, String)>,
    /// Vision models from models.toml that are downloaded.
    pub models: Vec<String>,
}

/// One change from the window.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingsChange {
    Voice {
        engine: String,
        kokoro_voice: String,
        system_voice: Option<String>,
        speed: f32,
    },
    Length(ResponseLength),
    MathAsWords(bool),
    /// Preset name and custom text (used only by "custom").
    Personality {
        preset: String,
        custom: String,
    },
    Think {
        effort: String,
        idle_mins: u64,
    },
    /// Already registered by the platform crate when this arrives.
    Hotkeys([String; 3]),
    /// "glance" or "system"; the platform crate repaints.
    Accent(String),
    /// Applied by restarting glance.
    Models {
        fast: String,
        deep: Option<String>,
        auto_escalate: bool,
        thinking_budget: Option<u32>,
    },
}

/// How a change took effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    Live,
    /// Saved; takes effect when glance restarts.
    NeedsRestart,
}

/// Implemented by the app: read the settings, save and apply changes.
pub trait SettingsBackend: Send + Sync {
    fn current(&self) -> (SettingsValues, SettingsChoices);
    /// What a fresh config.toml would hold ("Restore defaults").
    fn defaults(&self) -> SettingsValues;
    /// Save the change to config.toml and apply what can be applied live.
    /// The error text is shown in the window.
    fn apply(&self, change: SettingsChange) -> Result<Applied, String>;
}

/// One model in `models.toml`, as the models panel shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelRow {
    pub id: String,
    /// "vision", "speech-to-text" or "voice".
    pub role: String,
    pub license: String,
    /// Hugging Face repo and the pinned commit it downloads from.
    pub repo: String,
    pub revision: String,
    /// All files, and what is still missing on disk.
    pub bytes: u64,
    pub bytes_missing: u64,
    pub state: ModelState,
    /// What uses it ("FAST model", "speech recognition", …); such a model
    /// cannot be deleted.
    pub in_use: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ModelState {
    NotDownloaded,
    /// Some files are there (an earlier download stopped).
    Partial {
        present: usize,
        total: usize,
    },
    /// All files are there with their pinned sizes; not hashed this session
    /// (every load hashes them anyway).
    Downloaded,
    /// Hashed this session: every file matches its pinned SHA-256.
    Verified,
    /// The last download or check failed.
    Failed(String),
    /// Checking the files already there before downloading the rest.
    Preparing,
    Downloading {
        done: u64,
        total: u64,
    },
    Verifying {
        file: usize,
        files: usize,
    },
}

impl ModelState {
    pub fn is_busy(&self) -> bool {
        matches!(
            self,
            ModelState::Preparing | ModelState::Downloading { .. } | ModelState::Verifying { .. }
        )
    }
}

/// Implemented by the app: list, download, verify and delete models, only
/// ever those pinned in `models.toml`. One job runs at a time.
pub trait ModelsBackend: Send + Sync {
    fn rows(&self) -> Vec<ModelRow>;
    /// Free space where models are stored.
    fn free_bytes(&self) -> Option<u64>;
    /// Start downloading the missing files (the pinned downloader verifies
    /// each one). Returns at once; progress shows in [`rows`](Self::rows).
    fn download(&self, id: &str) -> Result<(), String>;
    /// Start re-hashing the files against their pins.
    fn verify(&self, id: &str) -> Result<(), String>;
    /// Stop the running job; an unfinished download's partial file is removed.
    fn cancel(&self);
    /// Delete the files. Refused while the model is in use or busy.
    fn delete(&self, id: &str) -> Result<u64, String>;
}
