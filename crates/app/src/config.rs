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
//! ```

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use glance_tts::kokoro::VoiceChoice;
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub speech: SpeechConfig,
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
    fn typos_and_bad_values_are_errors() {
        assert!(Config::parse("[speech]\nvoise = \"af_heart\"\n").is_err());
        assert!(Config::parse("[speech]\nspeed = 3.0\n").is_err());
        assert!(Config::parse("[speech]\nkokoro_model = \"fp16\"\n").is_err());
        assert!(Config::parse("[speech]\nvoice = \"../x\"\n").is_err());
        assert!(Config::parse("[speach]\n").is_err());
    }
}
