//! The setup window's backend: what this Mac is, which preset fits it
//! (DESIGN §7.4), the downloads (through the models backend, so the same
//! pinned downloader), the test question, and the setup state file.
//!
//! State lives in `~/Library/Application Support/glance/state/setup.toml`,
//! beside `run/` and apart from config.toml: whether setup is complete,
//! the preset chosen, the page it was on, and whether the Screen Recording
//! prompt was shown. Nothing personal. It is parsed strictly; a damaged
//! file counts as "not set up", which only shows the window again.
//!
//! config.toml is written once, through `config_edit` and the strict
//! parser, when the chosen preset's models are on disk: `[models] fast` and
//! `deep`. So config never names a model that is not there, and quitting
//! during setup leaves it as it was.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use glance_orchestrator::settings::{DownloadIssue, DownloadPhase, ModelsBackend};
use glance_orchestrator::setup::{
    MachineInfo, PresetInfo, RamTier, SetupBackend, SetupSnapshot, Skip, Step, TestResult,
};
use serde::Deserialize;

use super::super::{KOKORO_MODELS, KOKORO_VOICES, manifest, store};
use super::models::{DISK_MARGIN, free_bytes};
use crate::config::{Config, Engine};
use crate::config_edit::{self, Value};

/// One setup choice (DESIGN §7.4). Memory figures are the measured ones
/// from the 48 GB reference machine (`glance fast-bench`, `deep-bench`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub summary: &'static str,
    pub fast: &'static str,
    pub deep: Option<&'static str>,
    /// Offered from this tier up.
    pub from: RamTier,
    pub memory: &'static str,
}

pub const BASICS: &str = "basics";

pub const PRESETS: [Preset; 3] = [
    Preset {
        id: BASICS,
        name: "Basics",
        summary: "One small vision model. Fast answers about what is on screen.",
        fast: "qwen3-vl-4b-instruct-q4km",
        deep: None,
        from: RamTier::BelowMinimum,
        memory: "about 7.0 GB",
    },
    Preset {
        id: "sharper",
        name: "Sharper reading",
        summary: "A larger vision model in place of the small one.",
        fast: "qwen3-vl-8b-instruct-q4km",
        deep: None,
        from: RamTier::Gb32,
        memory: "about 9.8 GB",
    },
    Preset {
        id: "think",
        name: "Basics + think harder",
        summary: "The small model, plus a large one that loads only when you ask it to think \
                  harder, and unloads when idle.",
        fast: "qwen3-vl-4b-instruct-q4km",
        deep: Some("qwen3.8-27b-q4km"),
        from: RamTier::Gb48,
        memory: "about 7.0 GB; 26.4 GB while think harder is loaded",
    },
];

fn rank(tier: RamTier) -> u8 {
    match tier {
        RamTier::BelowMinimum => 0,
        RamTier::Gb16 => 1,
        RamTier::Gb32 => 2,
        RamTier::Gb48 => 3,
    }
}

pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// What the Your Mac page offers and recommends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recommendation {
    pub recommended: &'static str,
    pub offered: Vec<&'static str>,
    pub warning: Option<Warning>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Warning {
    /// Under 16 GB: not supported; only Basics is offered.
    BelowMinimum,
    /// RAM could not be read: only Basics is offered.
    UnknownRam,
    /// Even the recommended preset does not fit on the disk.
    NotEnoughDisk,
}

/// Basics is recommended on every tier (maintainer, 2026-11-27); larger
/// presets are offered from their tier up. `fits(preset)` says whether its
/// missing bytes plus the 2 GB margin fit on the disk.
pub fn recommend(tier: Option<RamTier>, fits: impl Fn(&Preset) -> bool) -> Recommendation {
    let offered: Vec<&'static str> = match tier {
        None | Some(RamTier::BelowMinimum) => vec![BASICS],
        Some(tier) => PRESETS
            .iter()
            .filter(|p| rank(p.from) <= rank(tier))
            .map(|p| p.id)
            .collect(),
    };
    let basics = &PRESETS[0];
    let warning = match tier {
        None => Some(Warning::UnknownRam),
        Some(RamTier::BelowMinimum) => Some(Warning::BelowMinimum),
        _ if !fits(basics) => Some(Warning::NotEnoughDisk),
        _ => None,
    };
    Recommendation {
        recommended: BASICS,
        offered,
        warning,
    }
}

/// Friendly names for the models a preset includes.
fn model_label(id: &str) -> String {
    match id {
        "qwen3-vl-4b-instruct-q4km" => "Vision: Qwen3-VL 4B".into(),
        "qwen3-vl-8b-instruct-q4km" => "Vision: Qwen3-VL 8B".into(),
        "qwen3.8-27b-q4km" => "Think harder: Qwen3.8 27B, loaded on demand".into(),
        "parakeet-tdt-0.6b-v3-int8" => "Speech-to-text: Parakeet v3".into(),
        "parakeet-tdt-0.6b-v2-int8" => "Speech-to-text: Parakeet v2 (English)".into(),
        "kokoro-82m-v1.0-fp32" => "Voice: Kokoro".into(),
        KOKORO_VOICES => "Voices: 28 English Kokoro voices".into(),
        other => other.to_owned(),
    }
}

/// The Kokoro model id the config uses.
fn kokoro_id(config: &Config) -> &'static str {
    KOKORO_MODELS
        .iter()
        .find(|(short, _)| *short == config.speech.kokoro_model)
        .map_or(KOKORO_MODELS[0].1, |(_, id)| id)
}

/// The models a configuration runs, with what each is for. The speech
/// model comes from `--stt`.
pub fn configured_models(config: &Config, stt: &str) -> Vec<(String, String)> {
    let mut list = vec![(stt.to_owned(), "speech recognition".to_owned())];
    let m = &config.models;
    if m.fast_endpoint.is_none() {
        list.push((m.fast.clone(), "FAST model".to_owned()));
    }
    if let (None, Some(deep)) = (&m.deep_endpoint, &m.deep) {
        list.push((deep.clone(), "DEEP model".to_owned()));
    }
    if config.speech.engine == Engine::Kokoro {
        list.push((kokoro_id(config).to_owned(), "voice".to_owned()));
        list.push((KOKORO_VOICES.to_owned(), "voices".to_owned()));
    }
    list
}

/// The models a preset needs, given the voice and speech model in use.
pub fn preset_models(preset: &Preset, config: &Config, stt: &str) -> Vec<String> {
    let mut ids = vec![preset.fast.to_owned()];
    ids.extend(preset.deep.map(str::to_owned));
    ids.push(stt.to_owned());
    if config.speech.engine == Engine::Kokoro {
        ids.push(kokoro_id(config).to_owned());
        ids.push(KOKORO_VOICES.to_owned());
    }
    ids
}

/// Every file of each model is on disk with its pinned size.
pub fn models_present(ids: &[String]) -> bool {
    let (Ok(manifest), Ok(store)) = (manifest(), store()) else {
        return false;
    };
    ids.iter().all(|id| {
        manifest
            .model(id)
            .is_some_and(|m| store.files_present(m) == m.files.len())
    })
}

/// `~/Library/Application Support/glance/state/setup.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupState {
    schema: u32,
    #[serde(default)]
    pub completed: bool,
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub step: Option<String>,
    #[serde(default)]
    pub screen_asked: bool,
}

impl SetupState {
    pub fn path() -> Result<PathBuf> {
        let home = std::env::var_os("HOME").context("HOME is not set")?;
        Ok(PathBuf::from(home).join("Library/Application Support/glance/state/setup.toml"))
    }

    /// Strict: an unknown key, another schema or a value glance never
    /// writes is an error.
    pub fn parse(text: &str) -> Result<Self> {
        let state: Self = toml::from_str(text)?;
        if state.schema != 1 {
            bail!("unknown setup state schema {}", state.schema);
        }
        if let Some(p) = &state.preset
            && preset(p).is_none()
        {
            bail!("unknown preset");
        }
        if let Some(s) = &state.step
            && Step::parse(s).is_none()
        {
            bail!("unknown step");
        }
        Ok(state)
    }

    /// The saved state; missing or damaged means "not set up".
    pub fn load() -> Self {
        let Ok(path) = Self::path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).unwrap_or_else(|e| {
                eprintln!("glance: setup: state file ignored ({e})");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn render(&self) -> String {
        // Only values from fixed sets are written (checked again by parse).
        let mut text = format!(
            "# glance's setup state (not settings: those are in config.toml)\nschema = 1\n\
             completed = {}\nscreen_asked = {}\n",
            self.completed, self.screen_asked
        );
        if let Some(p) = self.preset.as_deref().and_then(preset) {
            text.push_str(&format!("preset = \"{}\"\n", p.id));
        }
        if let Some(s) = self.step.as_deref().and_then(Step::parse) {
            text.push_str(&format!("step = \"{}\"\n", s.as_str()));
        }
        text
    }

    /// Write it whole: a temporary file, then a rename.
    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        let text = self.render();
        Self::parse(&text).context("setup state would not read back")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temp = path.with_extension("toml.new");
        std::fs::write(&temp, &text)?;
        std::fs::rename(&temp, &path)?;
        Ok(())
    }

    pub fn saved_step(&self) -> Option<Step> {
        self.step.as_deref().and_then(Step::parse)
    }
}

/// "Apple M4 Pro" and the RAM, from `sysctl`.
fn sysctl() -> (Option<String>, Option<u64>) {
    let out = std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", "machdep.cpu.brand_string", "hw.memsize"])
        .output();
    let Ok(out) = out else {
        return (None, None);
    };
    parse_sysctl(&String::from_utf8_lossy(&out.stdout))
}

fn parse_sysctl(text: &str) -> (Option<String>, Option<u64>) {
    let mut lines = text.lines().map(str::trim);
    let chip = lines.next().filter(|l| !l.is_empty()).map(str::to_owned);
    let ram = lines.next().and_then(|l| l.parse().ok());
    (chip, ram)
}

/// What the test question does, set by the app once the pipeline runs.
pub struct TestHooks {
    pub start: Box<dyn Fn() -> Result<(), String> + Send + Sync>,
    pub stop: Box<dyn Fn() + Send + Sync>,
}

pub struct AppSetup {
    models: Arc<dyn ModelsBackend>,
    stt: String,
    /// FAST is an endpoint given on the command line: no FAST download.
    fast_external: bool,
    /// The pipeline could not start (shown instead of a test).
    broken: Mutex<Option<String>>,
    state: Mutex<SetupState>,
    test: Mutex<TestResult>,
    hooks: OnceLock<TestHooks>,
    /// The FAST and DEEP the running pipeline started with.
    running: Mutex<Option<(String, Option<String>)>>,
    /// Fired whenever the configured models may have become ready.
    ready: Arc<tokio::sync::Notify>,
    machine: OnceLock<(Option<String>, Option<u64>)>,
    free: Mutex<Option<(Instant, Option<u64>)>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl AppSetup {
    pub fn new(models: Arc<dyn ModelsBackend>, stt: String, fast_external: bool) -> Arc<Self> {
        Arc::new(Self {
            models,
            stt,
            fast_external,
            broken: Mutex::new(None),
            state: Mutex::new(SetupState::load()),
            test: Mutex::new(TestResult::Idle),
            hooks: OnceLock::new(),
            running: Mutex::new(None),
            ready: Arc::new(tokio::sync::Notify::new()),
            machine: OnceLock::new(),
            free: Mutex::new(None),
        })
    }

    pub fn state(&self) -> SetupState {
        lock(&self.state).clone()
    }

    /// Notified when setup may have made the configured models ready.
    pub fn ready(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.ready)
    }

    pub fn set_hooks(&self, hooks: TestHooks) {
        let _ = self.hooks.set(hooks);
    }

    pub fn set_running(&self, fast: String, deep: Option<String>) {
        *lock(&self.running) = Some((fast, deep));
    }

    /// The pipeline failed to start: the test says so.
    pub fn set_broken(&self, message: String) {
        *lock(&self.broken) = Some(message);
    }

    pub fn set_test(&self, result: TestResult) {
        *lock(&self.test) = result;
    }

    pub fn test(&self) -> TestResult {
        lock(&self.test).clone()
    }

    fn update_state(&self, change: impl FnOnce(&mut SetupState)) {
        let mut state = lock(&self.state);
        change(&mut state);
        if let Err(e) = state.save() {
            eprintln!("glance: setup: could not save the setup state: {e:#}");
        }
    }

    fn free_bytes(&self) -> Option<u64> {
        let mut cached = lock(&self.free);
        if let Some((at, free)) = *cached
            && at.elapsed() < Duration::from_secs(5)
        {
            return free;
        }
        let free = store().ok().and_then(|s| free_bytes(s.root()));
        *cached = Some((Instant::now(), free));
        free
    }

    fn tier(&self) -> Option<RamTier> {
        self.machine.get_or_init(sysctl).1.map(RamTier::of)
    }

    fn chosen(&self, config: &Config) -> &'static Preset {
        let state = lock(&self.state);
        let offered = recommend(self.tier(), |_| true).offered;
        state
            .preset
            .as_deref()
            .and_then(preset)
            .filter(|p| offered.contains(&p.id))
            // Otherwise the preset matching the current config, if offered.
            .or_else(|| {
                PRESETS.iter().find(|p| {
                    offered.contains(&p.id)
                        && p.fast == config.models.fast
                        && p.deep.map(str::to_owned) == config.models.deep
                })
            })
            .unwrap_or(&PRESETS[0])
    }

    /// Write the chosen preset into config.toml, if it is not there yet.
    /// Only with its models on disk.
    fn apply_choice(&self) -> Result<(), String> {
        let config = Config::load().map_err(|e| format!("{e:#}"))?;
        let chosen = self.chosen(&config);
        if !models_present(&preset_models(chosen, &config, &self.stt)) {
            return Err("the models for this setup are not downloaded yet".into());
        }
        let deep = chosen.deep.map(str::to_owned);
        if config.models.fast == chosen.fast && config.models.deep == deep {
            return Ok(());
        }
        let path = Config::path().map_err(|e| format!("{e:#}"))?;
        config_edit::update(
            &path,
            &[
                ("models", "fast", Some(Value::Str(chosen.fast.to_owned()))),
                ("models", "deep", deep.map(Value::Str)),
            ],
            |text| Config::parse(text).map(|_| ()),
        )
        .map_err(|e| format!("{e:#}"))?;
        eprintln!("glance: setup: preset={} saved", chosen.id);
        Ok(())
    }

    /// What glance needs at launch (from config.toml) is on disk.
    pub fn configured_ready(&self) -> bool {
        let Ok(config) = Config::load() else {
            return false;
        };
        let ids: Vec<String> = configured_models(&config, &self.stt)
            .into_iter()
            .filter(|(_, label)| !(self.fast_external && label == "FAST model"))
            .map(|(id, _)| id)
            .collect();
        models_present(&ids)
    }
}

impl SetupBackend for AppSetup {
    fn snapshot(&self) -> SetupSnapshot {
        let config = Config::load().unwrap_or_default();
        let (chip, ram) = self.machine.get_or_init(sysctl).clone();
        let free = self.free_bytes();
        let tier = ram.map(RamTier::of);
        let rows = self.models.rows();
        let missing = |id: &str| {
            rows.iter()
                .find(|r| r.id == id)
                .map_or(0, |r| r.bytes_missing)
        };
        let total = |id: &str| rows.iter().find(|r| r.id == id).map_or(0, |r| r.bytes);
        let fits = |p: &Preset| {
            let need: u64 = preset_models(p, &config, &self.stt)
                .iter()
                .map(|id| missing(id))
                .sum();
            need == 0 || free.is_none_or(|f| f >= need + DISK_MARGIN)
        };
        let rec = recommend(tier, fits);
        let tested = tier.is_some_and(RamTier::tested);
        let presets = PRESETS
            .iter()
            .map(|p| {
                let ids = preset_models(p, &config, &self.stt);
                PresetInfo {
                    id: p.id.to_owned(),
                    name: p.name.to_owned(),
                    summary: p.summary.to_owned(),
                    includes: ids.iter().map(|id| model_label(id)).collect(),
                    total_bytes: ids.iter().map(|id| total(id)).sum(),
                    missing_bytes: ids.iter().map(|id| missing(id)).sum(),
                    memory: p.memory.to_owned(),
                    memory_note: if tested {
                        "Measured on a 48 GB M4 Pro.".to_owned()
                    } else {
                        "Measured on a 48 GB M4 Pro; untested on a Mac like this one.".to_owned()
                    },
                    offered: rec.offered.contains(&p.id),
                    fits_disk: fits(p),
                }
            })
            .collect();
        let chosen = self.chosen(&config);
        let ids = preset_models(chosen, &config, &self.stt);
        let download = self.models.progress();
        let running = download
            .as_ref()
            .is_some_and(|d| d.phase == DownloadPhase::Running);
        let models_ready = !running && models_present(&ids);
        let needs_restart = lock(&self.running)
            .as_ref()
            .is_some_and(|(fast, deep)| *fast != chosen.fast || deep.as_deref() != chosen.deep);
        let state = lock(&self.state).clone();
        SetupSnapshot {
            machine: MachineInfo {
                chip,
                ram_bytes: ram,
                free_bytes: free,
                llama_server: config
                    .models
                    .llama_server()
                    .map(|p| p.display().to_string())
                    .map_err(|e| format!("{e:#}")),
            },
            tier,
            presets,
            recommended: rec.recommended.to_owned(),
            chosen: chosen.id.to_owned(),
            models_ready,
            download,
            test: self.test(),
            hotkeys: [
                config.hotkeys.talk.clone(),
                config.hotkeys.look.clone(),
                config.hotkeys.think.clone(),
            ],
            voice: match config.speech.engine {
                Engine::Kokoro => format!("Kokoro ({})", config.speech.voice),
                Engine::System => "macOS voice".to_owned(),
            },
            needs_restart,
            screen_asked: state.screen_asked,
        }
    }

    fn choose(&self, id: &str) {
        if let Some(p) = preset(id) {
            self.update_state(|s| s.preset = Some(p.id.to_owned()));
            eprintln!("glance: setup: preset={} chosen", p.id);
        }
    }

    fn download(&self) -> Result<(), DownloadIssue> {
        let config = Config::load().unwrap_or_default();
        let ids = preset_models(self.chosen(&config), &config, &self.stt);
        // Persist the choice before a long download.
        let chosen = self.chosen(&config).id.to_owned();
        self.update_state(|s| s.preset = Some(chosen));
        let result = self.models.download_many(&ids);
        if let Err(e) = &result {
            eprintln!("glance: setup: download code={}", e.code());
        }
        result
    }

    fn pause(&self) {
        self.models.pause();
    }

    fn resume(&self) -> Result<(), DownloadIssue> {
        self.download()
    }

    fn cancel(&self) {
        self.models.cancel();
    }

    fn start_test(&self) -> Result<(), String> {
        if self.test().is_running() {
            return Ok(());
        }
        if let Some(broken) = lock(&self.broken).clone() {
            self.set_test(TestResult::Failed {
                stage: glance_orchestrator::setup::Stage::Model,
                hint: format!("glance could not start: {broken}"),
            });
            return Ok(());
        }
        let Some(hooks) = self.hooks.get() else {
            self.set_test(TestResult::Starting);
            return Err("glance is still starting its models; try again in a moment".into());
        };
        (hooks.start)()
    }

    fn stop_test(&self) {
        if let Some(hooks) = self.hooks.get() {
            (hooks.stop)();
        }
    }

    fn save_step(&self, step: Step) {
        eprintln!("glance: setup: step={}", step.as_str());
        // Leaving Download with the models there: write the choice now,
        // so the pipeline starts with it (and the test uses it).
        if matches!(step, Step::TryIt | Step::Done)
            && let Err(e) = self.apply_choice()
        {
            eprintln!("glance: setup: preset not saved: {e}");
        }
        self.update_state(|s| s.step = Some(step.as_str().to_owned()));
        self.ready.notify_waiters();
    }

    fn finish(&self) -> Result<(), String> {
        self.apply_choice()?;
        if !self.configured_ready() {
            return Err("the models are not all downloaded yet".into());
        }
        self.update_state(|s| {
            s.completed = true;
            s.step = None;
        });
        eprintln!("glance: setup: completed");
        self.ready.notify_waiters();
        Ok(())
    }

    fn skip(&self) -> Skip {
        let outcome = if self.configured_ready() {
            self.update_state(|s| {
                s.completed = true;
                s.step = None;
            });
            Skip::MarkComplete
        } else {
            Skip::CloseOnly
        };
        eprintln!(
            "glance: setup: skipped ({})",
            match outcome {
                Skip::MarkComplete => "complete",
                Skip::CloseOnly => "models missing; opens again next launch",
            }
        );
        outcome
    }

    fn restart(&self, step: Step) {
        self.update_state(|s| s.step = Some(step.as_str().to_owned()));
        eprintln!("glance: setup: restarting (back to step={})", step.as_str());
        glance_macos::shell::restart();
    }

    fn note_screen_asked(&self) {
        self.update_state(|s| s.screen_asked = true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1 << 30;

    /// RAM × disk: (RAM, what fits, recommended, offered, warning).
    #[test]
    fn recommendation_table() {
        type Row = (
            Option<u64>,
            &'static [&'static str],
            &'static [&'static str],
            Option<Warning>,
        );
        let all: &[&str] = &["basics", "sharper", "think"];
        let rows: &[Row] = &[
            (Some(8 * GB), all, &["basics"], Some(Warning::BelowMinimum)),
            (Some(8 * GB), &[], &["basics"], Some(Warning::BelowMinimum)),
            (Some(15 * GB), all, &["basics"], Some(Warning::BelowMinimum)),
            (Some(16 * GB), all, &["basics"], None),
            (Some(18 * GB), all, &["basics"], None),
            (
                Some(24 * GB),
                &[],
                &["basics"],
                Some(Warning::NotEnoughDisk),
            ),
            (Some(32 * GB), all, &["basics", "sharper"], None),
            (Some(36 * GB), &["basics"], &["basics", "sharper"], None),
            (
                Some(36 * GB),
                &[],
                &["basics", "sharper"],
                Some(Warning::NotEnoughDisk),
            ),
            (Some(48 * GB), all, &["basics", "sharper", "think"], None),
            (
                Some(48 * GB),
                &["basics", "sharper"],
                &["basics", "sharper", "think"],
                None,
            ),
            (
                Some(48 * GB),
                &[],
                &["basics", "sharper", "think"],
                Some(Warning::NotEnoughDisk),
            ),
            (Some(64 * GB), all, &["basics", "sharper", "think"], None),
            (Some(128 * GB), all, &["basics", "sharper", "think"], None),
            (None, all, &["basics"], Some(Warning::UnknownRam)),
        ];
        for (ram, fits, offered, warning) in rows {
            let rec = recommend(ram.map(RamTier::of), |p| fits.contains(&p.id));
            assert_eq!(rec.recommended, "basics", "{ram:?}");
            assert_eq!(rec.offered, *offered, "{ram:?} {fits:?}");
            assert_eq!(rec.warning, *warning, "{ram:?} {fits:?}");
        }
    }

    #[test]
    fn presets_use_only_pinned_models() {
        let manifest = manifest().unwrap();
        let config = Config::default();
        for p in &PRESETS {
            for id in preset_models(p, &config, "parakeet-tdt-0.6b-v3-int8") {
                assert!(manifest.model(&id).is_some(), "{} uses unpinned {id}", p.id);
                assert_ne!(model_label(&id), id, "{id} has no friendly name");
            }
        }
        // The system voice needs no Kokoro download.
        let system = Config::parse("[speech]\nengine = \"system\"\n").unwrap();
        assert_eq!(
            preset_models(&PRESETS[0], &system, "stt"),
            ["qwen3-vl-4b-instruct-q4km", "stt"]
        );
        assert_eq!(
            preset_models(&PRESETS[2], &config, "stt")[..2],
            ["qwen3-vl-4b-instruct-q4km", "qwen3.8-27b-q4km"]
        );
    }

    #[test]
    fn configured_models_follow_the_config() {
        let config = Config::default();
        let ids: Vec<String> = configured_models(&config, "stt")
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            ids,
            [
                "stt",
                "qwen3-vl-4b-instruct-q4km",
                "kokoro-82m-v1.0-fp32",
                KOKORO_VOICES
            ]
        );
        let external =
            Config::parse("[models]\nfast_endpoint = \"http://127.0.0.1:8080/v1\"\n").unwrap();
        assert!(
            !configured_models(&external, "stt")
                .iter()
                .any(|(id, _)| id.starts_with("qwen"))
        );
    }

    #[test]
    fn the_state_file_round_trips_and_is_strict() {
        let state = SetupState {
            schema: 1,
            completed: true,
            preset: Some("think".into()),
            step: Some("permissions".into()),
            screen_asked: true,
        };
        assert_eq!(SetupState::parse(&state.render()).unwrap(), state);
        assert!(SetupState::parse("schema = 1\n").is_ok());
        assert!(SetupState::parse("schema = 2\n").is_err());
        assert!(SetupState::parse("schema = 1\nextra = 1\n").is_err());
        assert!(SetupState::parse("schema = 1\npreset = \"huge\"\n").is_err());
        assert!(SetupState::parse("schema = 1\nstep = \"nowhere\"\n").is_err());
        assert!(SetupState::parse("").is_err());
        // Unknown values are never written.
        let odd = SetupState {
            schema: 1,
            preset: Some("x\"\ny".into()),
            ..SetupState::default()
        };
        assert!(SetupState::parse(&odd.render()).unwrap().preset.is_none());
    }

    #[test]
    fn sysctl_output_is_read() {
        assert_eq!(
            parse_sysctl("Apple M4 Pro\n51539607552\n"),
            (Some("Apple M4 Pro".into()), Some(48 * GB))
        );
        assert_eq!(parse_sysctl(""), (None, None));
        assert_eq!(
            parse_sysctl("Apple M1\nnot a number\n"),
            (Some("Apple M1".into()), None)
        );
    }

    #[test]
    fn the_choice_is_saved_only_through_the_strict_parser() {
        // The edit setup makes, applied to a user's file, keeps every other
        // line and parses back; a broken file is never made worse.
        let file = "# mine\n[speech]\nvoice = \"bf_emma\"\n\n[models]\nfast = \"qwen3-vl-4b-instruct-q4km\"\ndeep = \"x\"\n";
        let mut text = config_edit::set_in(
            file,
            "models",
            "fast",
            Some(&Value::Str("qwen3-vl-8b-instruct-q4km".into())),
        );
        text = config_edit::set_in(&text, "models", "deep", None);
        assert_eq!(
            text,
            "# mine\n[speech]\nvoice = \"bf_emma\"\n\n[models]\nfast = \"qwen3-vl-8b-instruct-q4km\"\n"
        );
        let parsed = Config::parse(&text).unwrap();
        assert_eq!(parsed.models.fast, "qwen3-vl-8b-instruct-q4km");
        assert!(parsed.models.deep.is_none());
        assert_eq!(parsed.speech.voice, "bf_emma");
    }
}
