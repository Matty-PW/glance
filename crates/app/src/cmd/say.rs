//! `glance say` and `glance voices`: exercise text-to-speech on its own, so
//! voices can be auditioned from the terminal with the same text.

use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use glance_tts::system_voice::{installed_voices, run_main_loop_until};
use glance_tts::{ChunkerConfig, SentenceChunker};

use super::speech::{self, SpeechArgs};
use super::{KOKORO_VOICES, find_model, manifest, ms, store, track_speech};

#[derive(clap::Args)]
pub struct Args {
    /// Text to speak. It is split into sentences exactly as a model reply would be.
    #[arg(required = true)]
    text: Vec<String>,
    #[command(flatten)]
    speech: SpeechArgs,
    /// Cut speech off after this many milliseconds (to test stopping).
    #[arg(long)]
    stop_after_ms: Option<u64>,
}

#[derive(clap::Subcommand)]
pub enum VoicesAction {
    /// List Kokoro voices and the English system voices.
    List,
}

pub fn run(args: Args) -> Result<()> {
    let text = args.text.join(" ");
    let mut chunker = SentenceChunker::new(ChunkerConfig::default());
    let mut sentences = chunker.push(&text);
    sentences.extend(chunker.finish());
    if sentences.is_empty() {
        bail!("nothing speakable in that text");
    }

    let settings = args.speech.settings()?;
    let speech = speech::start(&settings);
    println!("engine             {}", speech.description);
    let voice = speech.tts;
    let progress = track_speech(speech.events);

    let started = Instant::now();
    for sentence in &sentences {
        voice.speak(sentence)?;
    }
    if let Some(after) = args.stop_after_ms {
        let voice = std::sync::Arc::clone(&voice);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(after));
            voice.stop();
        });
    }

    // The system voice reports on the main run loop; Kokoro does not need
    // it, but turning it does no harm.
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

pub fn voices(_action: Option<VoicesAction>) -> Result<()> {
    let manifest = manifest()?;
    let model = find_model(&manifest, KOKORO_VOICES)?;
    let store = store()?;
    let mut installed = 0;
    println!("Kokoro voices ({KOKORO_VOICES}); use with --voice <id>");
    println!("  {:<14} {:<9} {:<7} status", "id", "accent", "gender");
    for file in &model.files {
        let Some(id) = file
            .path
            .strip_prefix("voices/")
            .and_then(|p| p.strip_suffix(".bin"))
        else {
            continue;
        };
        let accent = if id.starts_with('b') {
            "British"
        } else {
            "American"
        };
        let gender = if id.as_bytes().get(1) == Some(&b'm') {
            "male"
        } else {
            "female"
        };
        // Presence only; files are hashed when a voice is loaded.
        let present = store.file_path(model, file).is_file();
        installed += usize::from(present);
        let status = if present {
            "installed"
        } else {
            "not downloaded"
        };
        println!("  {id:<14} {accent:<9} {gender:<7} {status}");
    }
    if installed < model.files.len() {
        println!("  run `glance models pull {KOKORO_VOICES}` to download them");
    }
    println!("  Blend two: --voice af_heart:0.6,bf_emma:0.4");

    let mut voices = installed_voices();
    voices.sort_by(|a, b| (&a.language, &a.name).cmp(&(&b.language, &b.name)));
    println!("\nSystem voices (English); use with --engine system --voice <identifier>");
    println!(
        "  {:<8} {:<9} {:<22} identifier",
        "language", "quality", "name"
    );
    for v in voices.iter().filter(|v| v.language.starts_with("en")) {
        println!(
            "  {:<8} {:<9} {:<22} {}",
            v.language, v.quality, v.name, v.identifier
        );
    }
    let others = voices
        .iter()
        .filter(|v| !v.language.starts_with("en"))
        .count();
    println!("  ({others} voices in other languages not shown)");
    Ok(())
}
