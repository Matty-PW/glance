//! `glance say` and `glance voices`: exercise text-to-speech on its own.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use glance_tts::system_voice::{
    SystemVoice, SystemVoiceConfig, installed_voices, run_main_loop_until,
};
use glance_tts::{ChunkerConfig, SentenceChunker, TextToSpeech};

use super::{ms, track_speech};

#[derive(clap::Args)]
pub struct Args {
    /// Text to speak. It is split into sentences exactly as a model reply would be.
    #[arg(required = true)]
    text: Vec<String>,
    #[command(flatten)]
    voice: VoiceArgs,
    /// Cut speech off after this many milliseconds (to test stopping).
    #[arg(long)]
    stop_after_ms: Option<u64>,
}

/// Voice options shared by every command that speaks.
#[derive(clap::Args, Clone)]
pub struct VoiceArgs {
    /// Voice identifier from `glance voices`, or a language code such as en-GB.
    #[arg(long)]
    pub voice: Option<String>,
    /// Speaking rate, 0.0 (slow) to 1.0 (fast). Default: the system's (0.5).
    #[arg(long)]
    pub rate: Option<f32>,
    /// Volume, 0.0 (silent) to 1.0.
    #[arg(long, default_value_t = 1.0)]
    pub volume: f32,
}

impl VoiceArgs {
    pub fn config(&self) -> SystemVoiceConfig {
        SystemVoiceConfig {
            voice: self.voice.clone(),
            rate: self.rate,
            volume: self.volume,
        }
    }
}

pub fn run(args: Args) -> Result<()> {
    let text = args.text.join(" ");
    let mut chunker = SentenceChunker::new(ChunkerConfig::default());
    let mut sentences = chunker.push(&text);
    sentences.extend(chunker.finish());
    if sentences.is_empty() {
        bail!("nothing speakable in that text");
    }

    let (voice, events) = SystemVoice::new(args.voice.config());
    let voice = Arc::new(voice);
    let progress = track_speech(events);

    let started = Instant::now();
    for sentence in &sentences {
        voice.speak(sentence)?;
    }
    if let Some(after) = args.stop_after_ms {
        let voice = Arc::clone(&voice);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(after));
            voice.stop();
        });
    }

    let total = sentences.len() as u64;
    let deadline = started + Duration::from_secs(120);
    run_main_loop_until(|| progress.borrow().ended >= total || Instant::now() > deadline);

    let seen = *progress.borrow();
    println!("sentences          {total}");
    match seen.first_audio {
        Some(at) => println!(
            "first audio        {:.0} ms after queueing",
            ms(at - started)
        ),
        None => println!("first audio        never reported"),
    }
    println!(
        "finished           {} spoken, {} cancelled, in {:.0} ms",
        seen.ended - seen.cancelled,
        seen.cancelled,
        ms(started.elapsed())
    );
    if seen.ended < total {
        bail!("speech did not finish within the time limit");
    }
    Ok(())
}

pub fn voices() -> Result<()> {
    let mut voices = installed_voices();
    voices.sort_by(|a, b| (&a.language, &a.name).cmp(&(&b.language, &b.name)));
    println!(
        "{:<8} {:<9} {:<22} identifier",
        "language", "quality", "name"
    );
    for v in voices.iter().filter(|v| v.language.starts_with("en")) {
        println!(
            "{:<8} {:<9} {:<22} {}",
            v.language, v.quality, v.name, v.identifier
        );
    }
    let others = voices
        .iter()
        .filter(|v| !v.language.starts_with("en"))
        .count();
    println!("({others} voices in other languages not shown)");
    Ok(())
}
