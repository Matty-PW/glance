//! What the settings window shows and changes, as plain data, and the
//! [`SettingsBackend`] the app implements. The window itself (native, in the
//! platform crate) sees only these types; the config file and the running
//! pipeline stay on the app's side.
//!
//! Scope (maintainer, 2026-11-18): voice, answer length, models,
//! think-harder options and hotkeys. Everything else is edited in
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
    pub length: ResponseLength,
    pub fast_model: String,
    pub deep_model: Option<String>,
    pub auto_escalate: bool,
    /// "off", "low", "medium" or "xhigh".
    pub deep_effort: String,
    pub deep_thinking_budget: Option<u32>,
    pub deep_idle_mins: u64,
    pub hotkeys: [String; 3],
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
    Think {
        effort: String,
        idle_mins: u64,
    },
    /// Already registered by the platform crate when this arrives.
    Hotkeys([String; 3]),
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
    /// Save the change to config.toml and apply what can be applied live.
    /// The error text is shown in the window.
    fn apply(&self, change: SettingsChange) -> Result<Applied, String>;
}
