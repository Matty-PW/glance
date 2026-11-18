//! Choosing and starting the speech engine, shared by `glance app`,
//! `glance say` and `glance spike`. Settings come from the config file, then
//! command-line flags override them. If Kokoro cannot start (model not
//! downloaded, files fail verification), glance falls back to the system
//! voice and says so.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use glance_macos::system_voice::{SystemVoice, SystemVoiceConfig, installed_voices};
use glance_tts::kokoro::{KokoroConfig, KokoroVoice, VoiceChoice};
use glance_tts::{SpeechEvent, TextToSpeech};
use tokio::sync::mpsc::UnboundedReceiver;

use super::{KOKORO_VOICES, find_model, kokoro_model_paths, kokoro_voices_dir, manifest};
use crate::config::{Config, Engine, SpeechConfig};

/// Speech flags shared by every command that speaks. Each one overrides
/// the config file.
#[derive(clap::Args, Clone, Default)]
pub struct SpeechArgs {
    /// kokoro or system (default from the config file: kokoro).
    #[arg(long, value_enum)]
    pub engine: Option<Engine>,
    /// A Kokoro voice (af_heart), a blend (af_heart:0.6,bf_emma:0.4), or a
    /// system voice identifier or language (en-GB). See `glance voices`.
    #[arg(long)]
    pub voice: Option<String>,
    /// Kokoro pace, 0.5 to 2.0 (1.0 is normal).
    #[arg(long)]
    pub speed: Option<f32>,
    /// System voice rate, 0.0 (slow) to 1.0 (fast).
    #[arg(long)]
    pub rate: Option<f32>,
    /// Volume, 0.0 (silent) to 1.0.
    #[arg(long)]
    pub volume: Option<f32>,
    /// Kokoro model: fp32 or q8f16.
    #[arg(long)]
    pub kokoro_model: Option<String>,
}

impl SpeechArgs {
    /// The config file's speech settings with these flags applied.
    pub fn settings(&self) -> Result<SpeechConfig> {
        let mut s = Config::load()?.speech;
        if let Some(voice) = &self.voice {
            // Without --engine, the voice says which engine is meant: Kokoro
            // ids look like af_heart; system voices are identifiers with dots
            // or language codes with a dash.
            let kokoro = VoiceChoice::parse(voice).is_ok();
            match (self.engine, kokoro) {
                (Some(Engine::System), _) | (None, false) => {
                    s.engine = Engine::System;
                    s.system_voice = Some(voice.clone());
                }
                _ => {
                    s.engine = Engine::Kokoro;
                    s.voice = voice.clone();
                }
            }
        }
        if let Some(engine) = self.engine {
            s.engine = engine;
        }
        if let Some(speed) = self.speed {
            s.speed = speed;
        }
        if let Some(rate) = self.rate {
            s.system_rate = Some(rate);
        }
        if let Some(volume) = self.volume {
            s.volume = volume;
        }
        if let Some(model) = &self.kokoro_model {
            s.kokoro_model = model.clone();
        }
        s.validate()?;
        if s.engine == Engine::System
            && let Some(voice) = &s.system_voice
        {
            check_system_voice(voice)?;
        }
        if s.engine == Engine::Kokoro {
            check_kokoro_voices(&s.voice_choice()?)?;
        }
        Ok(s)
    }
}

/// Every voice in a Kokoro choice must be one pinned in `models.toml`.
fn check_kokoro_voices(choice: &VoiceChoice) -> Result<()> {
    let manifest = manifest()?;
    let model = find_model(&manifest, KOKORO_VOICES)?;
    for id in choice.ids() {
        let path = format!("voices/{id}.bin");
        if !model.files.iter().any(|f| f.path == path) {
            bail!("`{id}` is not a Kokoro voice; see `glance voices`");
        }
    }
    Ok(())
}

/// A system voice must be an installed voice's identifier or the language
/// of one; anything else would make macOS quietly use its default voice.
fn check_system_voice(voice: &str) -> Result<()> {
    let known = installed_voices()
        .iter()
        .any(|v| v.identifier == voice || v.language.eq_ignore_ascii_case(voice));
    if !known {
        bail!(
            "`{voice}` is neither a Kokoro voice nor an installed system voice or language; \
             see `glance voices`"
        );
    }
    Ok(())
}

/// A running speech engine.
pub struct Speech {
    pub tts: Arc<dyn TextToSpeech>,
    pub events: UnboundedReceiver<SpeechEvent>,
    /// For log lines: "kokoro fp32, voice af_heart, speed 1".
    pub description: String,
}

/// Start the configured engine. Blocks while Kokoro loads (about a second);
/// call it from a blocking context.
pub fn start(settings: &SpeechConfig) -> Speech {
    if settings.engine == Engine::Kokoro {
        match start_kokoro(settings) {
            Ok(speech) => return speech,
            Err(e) => eprintln!("glance: Kokoro unavailable ({e:#}); using the system voice"),
        }
    }
    let (voice, events) = SystemVoice::new(SystemVoiceConfig {
        voice: settings.system_voice.clone(),
        rate: settings.system_rate,
        volume: settings.volume,
    });
    Speech {
        tts: Arc::new(voice),
        events,
        description: format!(
            "system voice {}",
            settings.system_voice.as_deref().unwrap_or("(default)")
        ),
    }
}

fn start_kokoro(settings: &SpeechConfig) -> Result<Speech> {
    let voice = settings.voice_choice()?;
    let (onnx, tokenizer) = kokoro_model_paths(&settings.kokoro_model)?;
    let config = KokoroConfig {
        onnx,
        tokenizer,
        voices_dir: kokoro_voices_dir()?,
        voice: voice.clone(),
        speed: settings.speed,
        threads: settings.threads,
        volume: settings.volume,
        idle_close: Duration::from_secs(30),
    };
    let (kokoro, events) = KokoroVoice::start(config)?;
    Ok(Speech {
        tts: Arc::new(kokoro),
        events,
        description: format!(
            "kokoro {}, voice {voice}, speed {}",
            settings.kokoro_model, settings.speed
        ),
    })
}
