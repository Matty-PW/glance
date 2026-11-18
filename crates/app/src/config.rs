//! `~/Library/Application Support/glance/config.toml`: settings that are not
//! secrets (API keys go in the Keychain, never here). A missing file means
//! defaults; an invalid one is an error that names the problem, so a typo
//! never silently changes behaviour. A settings UI arrives in M5.
//!
//! ```toml
//! [speech]
//! engine = "kokoro"        # or "system" (the macOS voice)
//! kokoro_model = "fp32"    # or "q8f16" (smaller, about 3x slower)
//! voice = "af_heart"       # see `glance voices`; a blend: "af_heart:0.6,bf_emma:0.4"
//! speed = 1.0              # Kokoro pace, 0.5 to 2.0
//! volume = 1.0             # 0.0 to 1.0
//! threads = 10             # ONNX Runtime threads for Kokoro
//! system_voice = "en-GB"   # for engine = "system": identifier or language
//! system_rate = 0.5        # 0.0 (slow) to 1.0 (fast)
//!
//! [models]
//! fast = "qwen3-vl-4b-instruct-q4km"  # a vision model from models.toml; glance runs it
//! deep = "…"                          # optional second, larger model (DEEP)
//! auto_escalate = false               # let FAST hand hard questions to DEEP by itself
//! # Or use servers you run yourself (loopback only); these override fast/deep:
//! # fast_endpoint = "http://127.0.0.1:8080/v1"
//! # fast_model = "default"
//! # deep_endpoint = "http://127.0.0.1:8081/v1"
//! # deep_model = "default"
//! fast_ctx = 8192
//! deep_ctx = 16384
//! deep_effort = "medium"        # Qwen3.8 reasoning effort: low, medium, xhigh, or off
//! deep_thinking_budget = 128    # cap on DEEP's thinking tokens (managed DEEP only)
//! deep_mtp = true               # speculative decoding with the model's MTP layers
//! ```

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use glance_tts::kokoro::VoiceChoice;
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub speech: SpeechConfig,
    pub models: ModelsConfig,
}

/// Where FAST and DEEP run (`docs/DESIGN.md` §7.1).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ModelsConfig {
    pub fast: String,
    pub fast_endpoint: Option<String>,
    pub fast_model: String,
    pub deep: Option<String>,
    pub deep_endpoint: Option<String>,
    pub deep_model: String,
    pub fast_ctx: u32,
    pub deep_ctx: u32,
    pub deep_effort: String,
    pub deep_thinking_budget: Option<u32>,
    pub deep_mtp: bool,
    /// FAST may escalate to DEEP on its own. Off by default: the default
    /// setup is one small model.
    pub auto_escalate: bool,
}

impl ModelsConfig {
    /// llama-server flags for a managed DEEP.
    pub fn deep_server_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if self.deep_mtp {
            args.extend(["--spec-type".to_owned(), "draft-mtp".to_owned()]);
        }
        if let Some(budget) = self.deep_thinking_budget {
            args.extend([
                "--reasoning-budget".to_owned(),
                budget.to_string(),
                "--reasoning-budget-message".to_owned(),
                "Time is up. Answer now, briefly.".to_owned(),
            ]);
        }
        args
    }

    /// Chat-template variables for every DEEP request (prewarm and question
    /// must match, or the cached prefix would not).
    pub fn deep_template_kwargs(&self) -> serde_json::Value {
        if self.deep_effort == "off" {
            serde_json::json!({"enable_thinking": false})
        } else {
            serde_json::json!({"reasoning_effort": self.deep_effort})
        }
    }
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            fast: "qwen3-vl-4b-instruct-q4km".into(),
            fast_endpoint: None,
            fast_model: "default".into(),
            deep: None,
            deep_endpoint: None,
            deep_model: "default".into(),
            fast_ctx: 8192,
            deep_ctx: 16384,
            deep_effort: "medium".into(),
            // M4 bake-off: 128 kept the answers checked correct at ~10 s to
            // the first word; 256 took ~16 s; off was fastest but wrong once.
            deep_thinking_budget: Some(128),
            deep_mtp: true,
            auto_escalate: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Kokoro,
    System,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SpeechConfig {
    pub engine: Engine,
    pub kokoro_model: String,
    pub voice: String,
    pub speed: f32,
    pub volume: f32,
    pub threads: usize,
    pub system_voice: Option<String>,
    pub system_rate: Option<f32>,
}

impl Default for SpeechConfig {
    fn default() -> Self {
        Self {
            engine: Engine::Kokoro,
            kokoro_model: "fp32".into(),
            voice: "af_heart".into(),
            speed: 1.0,
            volume: 1.0,
            // Best measured on the reference machine (10 performance cores);
            // more threads than performance cores is slower.
            threads: std::thread::available_parallelism()
                .map_or(4, |n| n.get())
                .min(10),
            system_voice: None,
            system_rate: None,
        }
    }
}

impl SpeechConfig {
    pub fn voice_choice(&self) -> Result<VoiceChoice> {
        VoiceChoice::parse(&self.voice).map_err(anyhow::Error::msg)
    }

    /// Check ranges and names; called after every override.
    pub fn validate(&self) -> Result<()> {
        if !(0.5..=2.0).contains(&self.speed) {
            bail!(
                "speech.speed must be between 0.5 and 2.0, got {}",
                self.speed
            );
        }
        if !(0.0..=1.0).contains(&self.volume) {
            bail!(
                "speech.volume must be between 0.0 and 1.0, got {}",
                self.volume
            );
        }
        if self.threads == 0 || self.threads > 64 {
            bail!("speech.threads must be between 1 and 64");
        }
        if let Some(rate) = self.system_rate
            && !(0.0..=1.0).contains(&rate)
        {
            bail!("speech.system_rate must be between 0.0 and 1.0, got {rate}");
        }
        match self.kokoro_model.as_str() {
            "fp32" | "q8f16" => {}
            "fp16" => bail!(
                "speech.kokoro_model = \"fp16\" does not work: that export produces only NaN \
                 samples on ONNX Runtime's CPU path. Use \"fp32\""
            ),
            other => bail!("speech.kokoro_model must be \"fp32\" or \"q8f16\", got \"{other}\""),
        }
        if self.engine == Engine::Kokoro {
            self.voice_choice()
                .with_context(|| format!("speech.voice = \"{}\"", self.voice))?;
        }
        Ok(())
    }
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        let home = std::env::var_os("HOME").context("HOME is not set")?;
        Ok(PathBuf::from(home).join("Library/Application Support/glance/config.toml"))
    }

    /// Read the config file, or defaults if there is none.
    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let config = Self::parse(&text).with_context(|| format!("in {}", path.display()))?;
        Ok(config)
    }

    pub fn parse(text: &str) -> Result<Self> {
        let config: Self = toml::from_str(text)?;
        config.speech.validate()?;
        if !["off", "low", "medium", "xhigh"].contains(&config.models.deep_effort.as_str()) {
            bail!(
                "models.deep_effort must be off, low, medium or xhigh, got \"{}\"",
                config.models.deep_effort
            );
        }
        for (name, ctx) in [
            ("fast_ctx", config.models.fast_ctx),
            ("deep_ctx", config.models.deep_ctx),
        ] {
            if !(2048..=131_072).contains(&ctx) {
                bail!("models.{name} must be between 2048 and 131072, got {ctx}");
            }
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_means_defaults() {
        let config = Config::parse("").unwrap();
        assert_eq!(config.speech.engine, Engine::Kokoro);
        assert_eq!(config.speech.voice, "af_heart");
        assert!((1..=10).contains(&config.speech.threads));
    }

    #[test]
    fn a_blend_and_the_system_voice_can_be_configured() {
        let config =
            Config::parse("[speech]\nvoice = \"af_heart:0.7,bm_george:0.3\"\nspeed = 1.1\n")
                .unwrap();
        assert_eq!(
            config.speech.voice_choice().unwrap().ids(),
            ["af_heart", "bm_george"]
        );
        let config =
            Config::parse("[speech]\nengine = \"system\"\nsystem_voice = \"en-GB\"\n").unwrap();
        assert_eq!(config.speech.engine, Engine::System);
    }

    #[test]
    fn models_default_to_a_managed_fast_tier_and_no_deep() {
        let config = Config::parse("").unwrap();
        assert_eq!(config.models.fast, "qwen3-vl-4b-instruct-q4km");
        assert!(config.models.deep.is_none() && config.models.fast_endpoint.is_none());
        assert!(!config.models.auto_escalate);
        let config = Config::parse(
            "[models]\ndeep_endpoint = \"http://127.0.0.1:8081/v1\"\ndeep_ctx = 32768\n",
        )
        .unwrap();
        assert_eq!(config.models.deep_ctx, 32768);
        assert!(Config::parse("[models]\nfast_ctx = 10\n").is_err());
        assert!(Config::parse("[models]\nfasst = \"x\"\n").is_err());
    }

    #[test]
    fn typos_and_bad_values_are_errors() {
        assert!(Config::parse("[speech]\nvoise = \"af_heart\"\n").is_err());
        assert!(Config::parse("[speech]\nspeed = 3.0\n").is_err());
        assert!(Config::parse("[speech]\nkokoro_model = \"fp16\"\n").is_err());
        assert!(Config::parse("[speech]\nvoice = \"../x\"\n").is_err());
        assert!(Config::parse("[speach]\n").is_err());
    }
}
