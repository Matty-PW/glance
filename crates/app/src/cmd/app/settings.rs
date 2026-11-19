//! The settings window's backend: reads `config.toml`, saves changes into it
//! (keeping the rest of the file as written, see `config_edit`) and applies
//! what can change while glance runs.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Result;
use glance_macos::system_voice::installed_voices;
use glance_models::Role;
use glance_orchestrator::Controller;
use glance_orchestrator::settings::{
    Applied, SettingsBackend, SettingsChange, SettingsChoices, SettingsValues,
};
use glance_tts::kokoro::VoiceChoice;

use super::super::speech::SpeechControl;
use super::super::{KOKORO_VOICES, find_model, manifest, store};
use crate::config::{Config, Engine, ModelsConfig};
use crate::config_edit::{self, Value};

/// Set once the pipeline runs; until then changes are saved but not applied.
#[derive(Default)]
pub struct AppSettings {
    live: OnceLock<(Controller, SpeechControl)>,
}

impl AppSettings {
    pub fn attach(&self, controller: Controller, speech: SpeechControl) {
        let _ = self.live.set((controller, speech));
    }

    fn save(&self, edits: &[(&str, &str, Option<Value>)]) -> Result<(), String> {
        let path = Config::path().map_err(|e| format!("{e:#}"))?;
        config_edit::update(&path, edits, |text| Config::parse(text).map(|_| ()))
            .map_err(|e| format!("{e:#}"))
    }
}

fn str_value(s: &str) -> Option<Value> {
    Some(Value::Str(s.to_owned()))
}

impl SettingsBackend for AppSettings {
    fn current(&self) -> (SettingsValues, SettingsChoices) {
        let config = Config::load().unwrap_or_else(|e| {
            eprintln!("glance: settings: {e:#}; showing defaults");
            Config::default()
        });
        let m = &config.models;
        let values = SettingsValues {
            engine: match config.speech.engine {
                Engine::Kokoro => "kokoro",
                Engine::System => "system",
            }
            .to_owned(),
            kokoro_voice: config.speech.voice.clone(),
            system_voice: config.speech.system_voice.clone(),
            speed: config.speech.speed,
            math_as_words: config.speech.math_as_words,
            length: m.response_length(),
            personality: m.personality.clone(),
            personality_custom: m.personality_custom.clone(),
            fast_model: m.fast.clone(),
            deep_model: m.deep.clone(),
            auto_escalate: m.auto_escalate,
            deep_effort: m.deep_effort.clone(),
            deep_thinking_budget: m.thinking_budget(),
            deep_idle_mins: (m.deep_idle_secs / 60).max(1),
            hotkeys: [
                config.hotkeys.talk.clone(),
                config.hotkeys.look.clone(),
                config.hotkeys.think.clone(),
            ],
        };
        (values, choices())
    }

    fn apply(&self, change: SettingsChange) -> Result<Applied, String> {
        let live = self.live.get();
        match change {
            SettingsChange::Voice {
                engine,
                kokoro_voice,
                system_voice,
                speed,
            } => {
                VoiceChoice::parse(&kokoro_voice).map_err(|e| format!("voice: {e}"))?;
                self.save(&[
                    ("speech", "engine", str_value(&engine)),
                    ("speech", "voice", str_value(&kokoro_voice)),
                    (
                        "speech",
                        "system_voice",
                        system_voice.as_deref().and_then(str_value),
                    ),
                    ("speech", "speed", Some(Value::Float(f64::from(speed)))),
                ])?;
                let Some((_, speech)) = live else {
                    return Ok(Applied::NeedsRestart);
                };
                let wanted = if engine == "system" {
                    Engine::System
                } else {
                    Engine::Kokoro
                };
                if speech.engine() != wanted {
                    return Ok(Applied::NeedsRestart);
                }
                match speech {
                    SpeechControl::Kokoro(kokoro) => {
                        let choice =
                            VoiceChoice::parse(&kokoro_voice).map_err(|e| e.to_string())?;
                        kokoro
                            .set_voice(&choice)
                            .map_err(|e| format!("saved, but the voice did not load: {e}"))?;
                        kokoro.set_speed(speed);
                    }
                    SpeechControl::System(system) => system.set_voice(system_voice),
                }
                Ok(Applied::Live)
            }
            SettingsChange::MathAsWords(on) => {
                self.save(&[("speech", "math_as_words", Some(Value::Bool(on)))])?;
                live.map_or(Ok(Applied::NeedsRestart), |(controller, _)| {
                    controller.set_math_as_words(on);
                    Ok(Applied::Live)
                })
            }
            SettingsChange::Length(length) => {
                self.save(&[("models", "response_length", str_value(length.as_str()))])?;
                live.map_or(Ok(Applied::NeedsRestart), |(controller, _)| {
                    controller.set_response_length(length);
                    Ok(Applied::Live)
                })
            }
            SettingsChange::Personality { preset, custom } => {
                let personality = glance_orchestrator::Personality::parse(&preset, &custom)?;
                // Saved cleaned (one line), and kept for later even when
                // another preset is chosen.
                let custom = if custom.trim().is_empty() {
                    String::new()
                } else {
                    glance_orchestrator::personality::clean_custom(&custom)?
                };
                self.save(&[
                    ("models", "personality", str_value(&preset)),
                    ("models", "personality_custom", str_value(&custom)),
                ])?;
                live.map_or(Ok(Applied::NeedsRestart), |(controller, _)| {
                    controller.set_personality(personality);
                    Ok(Applied::Live)
                })
            }
            SettingsChange::Think { effort, idle_mins } => {
                self.save(&[
                    ("models", "deep_effort", str_value(&effort)),
                    (
                        "models",
                        "deep_idle_secs",
                        Some(Value::Int(idle_mins as i64 * 60)),
                    ),
                ])?;
                live.map_or(Ok(Applied::NeedsRestart), |(controller, _)| {
                    let kwargs = ModelsConfig {
                        deep_effort: effort,
                        ..ModelsConfig::default()
                    }
                    .deep_template_kwargs();
                    controller.set_deep_template_kwargs(kwargs);
                    controller.set_deep_idle(Duration::from_secs(idle_mins * 60));
                    Ok(Applied::Live)
                })
            }
            SettingsChange::Hotkeys([talk, look, think]) => {
                // The window registered them already.
                self.save(&[
                    ("hotkeys", "talk", str_value(&talk)),
                    ("hotkeys", "look", str_value(&look)),
                    ("hotkeys", "think", str_value(&think)),
                ])?;
                Ok(Applied::Live)
            }
            SettingsChange::Models {
                fast,
                deep,
                auto_escalate,
                thinking_budget,
            } => {
                self.save(&[
                    ("models", "fast", str_value(&fast)),
                    ("models", "deep", deep.as_deref().and_then(str_value)),
                    ("models", "auto_escalate", Some(Value::Bool(auto_escalate))),
                    (
                        "models",
                        "deep_thinking_budget",
                        Some(Value::Int(i64::from(thinking_budget.unwrap_or(0)))),
                    ),
                ])?;
                Ok(Applied::NeedsRestart)
            }
        }
    }
}

/// Kokoro voices on disk, English system voices, downloaded vision models.
/// Presence checks only: files are hashed when they are loaded.
fn choices() -> SettingsChoices {
    let mut choices = SettingsChoices::default();
    let (Ok(manifest), Ok(store)) = (manifest(), store()) else {
        return choices;
    };
    if let Ok(voices) = find_model(&manifest, KOKORO_VOICES) {
        choices.kokoro_voices = voices
            .files
            .iter()
            .filter(|f| store.file_path(voices, f).is_file())
            .filter_map(|f| {
                f.path
                    .strip_prefix("voices/")
                    .and_then(|p| p.strip_suffix(".bin"))
                    .map(str::to_owned)
            })
            .collect();
    }
    let mut system: Vec<_> = installed_voices()
        .into_iter()
        .filter(|v| v.language.starts_with("en"))
        .collect();
    system.sort_by(|a, b| (&a.language, &a.name).cmp(&(&b.language, &b.name)));
    choices.system_voices = system
        .into_iter()
        .map(|v| (v.identifier, format!("{} ({})", v.name, v.language)))
        .collect();
    choices.models = manifest
        .models()
        .iter()
        .filter(|m| m.role == Role::Vlm)
        .filter(|m| m.files.iter().all(|f| store.file_path(m, f).is_file()))
        .map(|m| m.id.clone())
        .collect();
    choices
}

pub fn backend() -> Arc<AppSettings> {
    static BACKEND: OnceLock<Arc<AppSettings>> = OnceLock::new();
    Arc::clone(BACKEND.get_or_init(Arc::default))
}
