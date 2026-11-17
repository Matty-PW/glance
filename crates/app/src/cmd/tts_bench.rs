//! `glance tts-bench`: compare the Kokoro precisions with each other and with
//! the system voice.
//!
//! 1. Synthesis, without the speaker: model load, then phonemize and
//!    synthesize times per sentence for each model and thread count.
//! 2. Time to first audio through the `TextToSpeech` trait: `speak()` until
//!    the first sample reaches the device, cold (output device closed) and
//!    warm. This is the TTS share of the app's key-up-to-first-audio time.
//!
//! The sentences are fixed text shaped like the first chunk of a spoken
//! reply. `--out` writes them as 24 kHz WAV files for a listening test; that
//! is the only file output, and it holds only this fixed text.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use glance_tts::kokoro::{
    Accent, KokoroConfig, KokoroModel, KokoroVoice, Misaki, Phonemizer, SAMPLE_RATE, VoiceChoice,
    load_voice,
};
use glance_tts::system_voice::{SystemVoice, SystemVoiceConfig};
use glance_tts::{SpeechEvent, TextToSpeech, UtteranceId};

use tokio::sync::mpsc::UnboundedReceiver;

use super::{KOKORO_MODELS, kokoro_model_paths, kokoro_voices_dir, ms};

/// First chunks as the app would speak them: one sentence or an early
/// clause, 5 to 70 characters.
pub const SENTENCES: &[&str] = &[
    "This is a code editor showing a Rust file.",
    "The build failed because one test timed out.",
    "You have three unread messages, the newest from Priya.",
    "Sure.",
    "The pod is stuck in CrashLoopBackOff on node ip-10-0-3-17.",
    "Lunch is at 12:15, and the review moved to 2:30.",
    "It's a pull request called Harbor-5G cleanup, with two approvals.",
    "The chart shows revenue rising from March to June.",
];

#[derive(clap::Args)]
pub struct Args {
    /// Kokoro models to compare (fp32, fp16, q8f16).
    #[arg(long, value_delimiter = ',', default_value = "fp32,fp16,q8f16")]
    models: Vec<String>,
    /// Kokoro voice, or a blend such as af_heart:0.6,bf_emma:0.4.
    #[arg(long, default_value = "af_heart")]
    voice: String,
    /// ONNX Runtime thread counts to try for synthesis.
    #[arg(long, value_delimiter = ',', default_value = "4,8,10")]
    threads: Vec<usize>,
    /// Thread count used for the time-to-first-audio runs.
    #[arg(long, default_value_t = 8)]
    ttfa_threads: usize,
    /// Playback volume during the time-to-first-audio runs (0 = silent; the
    /// device still runs, so timing is unaffected).
    #[arg(long, default_value_t = 0.0)]
    volume: f32,
    /// Write the sentences as WAV files here, one folder per model.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Skip the time-to-first-audio runs.
    #[arg(long)]
    no_ttfa: bool,
}

pub async fn run(args: Args) -> Result<()> {
    let voice = VoiceChoice::parse(&args.voice).map_err(anyhow::Error::msg)?;
    let voices_dir = kokoro_voices_dir()?;
    let models: Vec<(String, PathBuf, PathBuf)> = args
        .models
        .iter()
        .map(|name| {
            let short = KOKORO_MODELS
                .iter()
                .find(|(s, id)| s == name || id == name)
                .map(|(s, _)| (*s).to_owned())
                .with_context(|| format!("unknown Kokoro model `{name}` (fp32, fp16, q8f16)"))?;
            let (onnx, tokenizer) = kokoro_model_paths(name)?;
            Ok((short, onnx, tokenizer))
        })
        .collect::<Result<_>>()?;

    println!("== synthesis (no speaker), voice {voice}");
    println!(
        "{:<7} {:>4}  {:>8}  {:>9}  {:>10} {:>10} {:>10}  {:>6}  {:>8}",
        "model",
        "thr",
        "load",
        "g2p med",
        "synth med",
        "synth p90",
        "synth max",
        "x real",
        "bad smp"
    );
    {
        let models = models.clone();
        let voice = voice.clone();
        let voices_dir = voices_dir.clone();
        let threads = args.threads.clone();
        let out = args.out.clone();
        tokio::task::spawn_blocking(move || synthesis(&models, &voice, &voices_dir, &threads, out))
            .await??;
    }

    if args.no_ttfa {
        return Ok(());
    }
    println!(
        "\n== time to first audio: speak() until the first sample plays ({} sentences, volume {})",
        SENTENCES.len(),
        args.volume
    );
    println!(
        "{:<16} {:>8}  {:>9}  {:>9}  {:>9}",
        "engine", "load", "cold", "warm med", "warm max"
    );
    let started = Instant::now();
    let (system, events) = SystemVoice::new(SystemVoiceConfig {
        volume: args.volume,
        ..Default::default()
    });
    report_ttfa("system voice", started.elapsed(), &system, events, None).await?;
    drop(system);

    for (short, onnx, tokenizer) in &models {
        let config = KokoroConfig {
            onnx: onnx.clone(),
            tokenizer: tokenizer.clone(),
            voices_dir: voices_dir.clone(),
            voice: voice.clone(),
            speed: 1.0,
            threads: args.ttfa_threads,
            volume: args.volume,
            idle_close: Duration::from_secs(30),
        };
        let started = Instant::now();
        let (kokoro, events) =
            tokio::task::spawn_blocking(move || KokoroVoice::start(config)).await??;
        let kokoro = Arc::new(kokoro);
        let label = format!("kokoro {short} ({}t)", args.ttfa_threads);
        if let Err(e) =
            report_ttfa(&label, started.elapsed(), &*kokoro, events, Some(&kokoro)).await
        {
            println!("{label:<16} fails: {e:#}");
        }
    }
    println!(
        "\ncold = first sentence with the output device closed; warm = device already open \
         (the app opens it at key-down). Add these to the key-up-to-first-chunk time \
         (~0.4 s in M0/M1) for the app's time to first audio."
    );
    Ok(())
}

fn synthesis(
    models: &[(String, PathBuf, PathBuf)],
    voice: &VoiceChoice,
    voices_dir: &Path,
    threads: &[usize],
    out: Option<PathBuf>,
) -> Result<()> {
    let style = load_voice(voices_dir, voice)?;
    let phonemizer = Misaki::new(Accent::of_voice(voice.lead()));
    for (short, onnx, tokenizer) in models {
        for (n, &thread_count) in threads.iter().enumerate() {
            let row = || -> Result<()> {
                let started = Instant::now();
                let mut model = KokoroModel::load(onnx, tokenizer, thread_count)?;
                let load = started.elapsed();
                // One untimed run, so first-run allocation is not counted.
                let warm = phonemizer.phonemize(SENTENCES[0])?;
                model.synthesize(&warm, &style, 1.0)?;

                let (mut g2p, mut synth) = (Vec::new(), Vec::new());
                let (mut audio_s, mut synth_s) = (0.0, 0.0);
                // Samples the model returned as NaN or infinite.
                let mut bad = 0usize;
                for (i, sentence) in SENTENCES.iter().enumerate() {
                    let t = Instant::now();
                    let phonemes = phonemizer.phonemize(sentence)?;
                    g2p.push(t.elapsed());
                    let t = Instant::now();
                    let samples = model.synthesize(&phonemes, &style, 1.0)?;
                    let took = t.elapsed();
                    synth.push(took);
                    bad += samples.iter().filter(|s| !s.is_finite()).count();
                    audio_s += samples.len() as f64 / f64::from(SAMPLE_RATE);
                    synth_s += took.as_secs_f64();
                    if n == 0
                        && let Some(dir) = &out
                    {
                        let dir = dir.join(short);
                        std::fs::create_dir_all(&dir)?;
                        write_wav(&dir.join(format!("s{:02}.wav", i + 1)), &samples)?;
                        std::fs::write(dir.join(format!("s{:02}.txt", i + 1)), sentence)?;
                    }
                }
                g2p.sort();
                synth.sort();
                println!(
                    "{:<7} {:>4}  {:>6.0}ms  {:>7.1}ms  {:>8.0}ms {:>8.0}ms {:>8.0}ms  {:>5.1}x  {:>8}",
                    short,
                    thread_count,
                    ms(load),
                    ms(g2p[g2p.len() / 2]),
                    ms(synth[synth.len() / 2]),
                    ms(synth[(synth.len() * 9).div_ceil(10) - 1]),
                    ms(synth[synth.len() - 1]),
                    audio_s / synth_s,
                    bad
                );
                Ok(())
            };
            if let Err(e) = row() {
                println!("{short:<7} {thread_count:>4}  fails: {e:#}");
            }
        }
    }
    if let Some(dir) = &out {
        println!("samples written to {}", dir.display());
    }
    Ok(())
}

/// Measure speak() to Started for every sentence, then print one row.
async fn report_ttfa(
    label: &str,
    load: Duration,
    tts: &dyn TextToSpeech,
    mut events: UnboundedReceiver<SpeechEvent>,
    kokoro: Option<&Arc<KokoroVoice>>,
) -> Result<()> {
    let cold = first_audio(tts, &mut events, SENTENCES[0]).await?;
    if let Some(k) = kokoro {
        k.warm_up();
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let mut warm = Vec::new();
    for sentence in SENTENCES {
        warm.push(first_audio(tts, &mut events, sentence).await?);
    }
    warm.sort();
    println!(
        "{:<16} {:>6.0}ms  {:>7.0}ms  {:>7.0}ms  {:>7.0}ms",
        label,
        ms(load),
        ms(cold),
        ms(warm[warm.len() / 2]),
        ms(warm[warm.len() - 1])
    );
    Ok(())
}

/// Time from `speak()` to the sentence's first sound, then stop it.
pub async fn first_audio(
    tts: &dyn TextToSpeech,
    events: &mut UnboundedReceiver<SpeechEvent>,
    text: &str,
) -> Result<Duration> {
    while events.try_recv().is_ok() {}
    let started = Instant::now();
    let id = tts.speak(text)?;
    let (at, event) = wait_for(events, id, true).await.with_context(|| {
        format!("no audio within 10 s for \"{text}\" (is a main run loop running?)")
    })?;
    if !matches!(event, SpeechEvent::Started(_)) {
        bail!("the engine dropped \"{text}\" without playing it (see its error above)");
    }
    let took = at.duration_since(started);
    tts.stop();
    let _ = wait_for(events, id, false).await;
    Ok(took)
}

/// Wait for utterance `id` to start (`start = true`) or to end. An end
/// also stops a wait for the start, so a dropped sentence is noticed.
async fn wait_for(
    events: &mut UnboundedReceiver<SpeechEvent>,
    id: UtteranceId,
    start: bool,
) -> Option<(Instant, SpeechEvent)> {
    let wait = async {
        while let Some(event) = events.recv().await {
            let hit = match event {
                SpeechEvent::Started(e) => start && e == id,
                SpeechEvent::Finished(e) | SpeechEvent::Cancelled(e) => e == id,
            };
            if hit {
                return Some((Instant::now(), event));
            }
        }
        None
    };
    tokio::time::timeout(Duration::from_secs(10), wait)
        .await
        .ok()
        .flatten()
}

/// A 16-bit mono WAV at Kokoro's rate.
fn write_wav(path: &Path, samples: &[f32]) -> Result<()> {
    let data_len = u32::try_from(samples.len() * 2).context("clip too long")?;
    let mut bytes = Vec::with_capacity(44 + samples.len() * 2);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    bytes.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // byte rate
    bytes.extend_from_slice(&2u16.to_le_bytes()); // block align
    bytes.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32_767.0).round() as i16;
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    if bytes.len() < 44 {
        bail!("bad WAV header");
    }
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}
