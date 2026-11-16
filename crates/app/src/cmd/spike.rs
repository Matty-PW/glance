//! `glance spike`: the end-to-end run. Record the microphone, transcribe, send
//! a PNG plus the transcript to a local model, print the streamed reply while
//! speaking it sentence by sentence, and report how long every stage took.

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use glance_audio::{AudioClip, AudioInput, Microphone, resample};
use glance_capture::{PngFile, ScreenCapture};
use glance_llm_client::{
    ChatEvent, ChatModel, ChatRequest, ChatSummary, Endpoint, Message, OpenAiClient, Part,
};
use glance_stt::{Parakeet, SpeechToText};
use serde_json::json;

use glance_tts::system_voice::SystemVoice;
use glance_tts::{ChunkerConfig, SentenceChunker, TextToSpeech};

use super::say::VoiceArgs;
use super::{SPOKEN_SYSTEM_PROMPT, find_model, manifest, ms, store, track_speech};

#[derive(clap::Args)]
pub struct Args {
    /// Screenshot to ask about (PNG). Read into memory, sent only to the endpoint.
    #[arg(long)]
    png: PathBuf,
    /// OpenAI-compatible API root, e.g. http://127.0.0.1:8080/v1 (loopback only).
    #[arg(long)]
    endpoint: String,
    /// Model name to request from the endpoint.
    #[arg(long, default_value = "default")]
    model: String,
    /// Speech model id from models.toml.
    #[arg(long, default_value = "parakeet-tdt-0.6b-v3-int8")]
    stt: String,
    /// How long to record the microphone.
    #[arg(long, default_value_t = 5.0)]
    seconds: f32,
    /// Use this 16 kHz mono WAV instead of the microphone (for automated runs).
    #[arg(long)]
    wav: Option<PathBuf>,
    /// Longest edge of the image sent to the model, in pixels.
    #[arg(long, default_value_t = 1024)]
    max_edge: u32,
    /// Do not send the image ahead of the question (to measure the difference).
    #[arg(long)]
    no_prewarm: bool,
    #[arg(long, default_value_t = 200)]
    max_tokens: u32,
    /// Print the reply without speaking it.
    #[arg(long)]
    no_speak: bool,
    #[command(flatten)]
    voice: VoiceArgs,
}

pub async fn run(args: Args) -> Result<()> {
    let mut report: Vec<(&str, String)> = Vec::new();
    fn stage(report: &mut Vec<(&str, String)>, name: &'static str, d: Duration) {
        report.push((name, format!("{:>8.1} ms", ms(d))));
    }

    // --- setup (not part of a turn) ------------------------------------------
    let manifest = manifest()?;
    let stt_model = find_model(&manifest, &args.stt)?;
    let started = Instant::now();
    let stt_dir = store()?
        .verified_dir(stt_model)
        .with_context(|| format!("run `glance models pull {}` first", stt_model.id))?;
    stage(&mut report, "stt verify (setup)", started.elapsed());
    let started = Instant::now();
    let mut stt = tokio::task::spawn_blocking(move || Parakeet::load(&stt_dir)).await??;
    stage(&mut report, "stt load (setup)", started.elapsed());

    let client = OpenAiClient::new(Endpoint::loopback(&args.endpoint, &args.model)?)?;

    // The speech engine is started up front so its start-up is not part of a turn.
    let speech = (!args.no_speak).then(|| {
        let (voice, events) = SystemVoice::new(args.voice.config());
        (voice, track_speech(events))
    });

    // --- "key down": capture, then prewarm while the user talks ---------------
    let started = Instant::now();
    let shot = PngFile::new(&args.png).capture()?;
    stage(&mut report, "capture (read+decode PNG)", started.elapsed());
    let started = Instant::now();
    let image = shot.encode(args.max_edge)?;
    stage(&mut report, "downscale + encode", started.elapsed());
    eprintln!(
        "image: {}x{} -> {}x{} ({} kB)",
        shot.width(),
        shot.height(),
        image.width(),
        image.height(),
        image.len() / 1000
    );

    let system = Message::system(SPOKEN_SYSTEM_PROMPT);
    let image_part = Part::Png(image.png());
    let prewarm = if args.no_prewarm {
        None
    } else {
        let client = client.clone();
        let messages = vec![system.clone(), Message::user(vec![image_part.clone()])];
        Some(tokio::spawn(async move {
            let started = Instant::now();
            let summary = complete(&client, messages, 1, |_| {}).await;
            (started.elapsed(), summary)
        }))
    };

    let clip = match &args.wav {
        Some(path) => {
            let clip = glance_stt::read_wav(path)?;
            // Stand in for the time the user would spend speaking, so the
            // prewarm overlaps it exactly as it does with the microphone.
            tokio::time::sleep(clip.duration()).await;
            clip
        }
        None => {
            let mut mic = Microphone::new(60);
            let started = Instant::now();
            mic.start()?;
            eprintln!(
                "Recording for {:.0} s: ask your question now.",
                args.seconds
            );
            tokio::time::sleep(Duration::from_secs_f32(args.seconds)).await;
            let recording = mic.stop()?;
            stage(&mut report, "record", started.elapsed());
            if let Some(latency) = recording.start_latency {
                stage(&mut report, "  mic start latency", latency);
            }
            if recording.clip.is_silent() {
                bail!(
                    "the microphone delivered pure silence. Grant Microphone access to your terminal in \
                     System Settings > Privacy & Security, then run again"
                );
            }
            recording.clip
        }
    };

    // --- "key up": everything from here counts toward time-to-first-audio -----
    let key_up = Instant::now();
    let started = Instant::now();
    let clip: AudioClip = resample(&clip, glance_stt::SAMPLE_RATE)?;
    stage(&mut report, "resample to 16 kHz", started.elapsed());

    let started = Instant::now();
    let transcript = tokio::task::spawn_blocking(move || stt.transcribe(&clip)).await??;
    stage(&mut report, "stt", started.elapsed());
    println!("you said: {}", transcript.text);
    if transcript.text.is_empty() {
        bail!("no speech was recognized");
    }

    if let Some(handle) = prewarm {
        let (elapsed, summary) = handle.await?;
        let summary = summary?;
        stage(&mut report, "prewarm (overlaps recording)", elapsed);
        if let Some(t) = summary.timings {
            report.push((
                "  prewarm tokens",
                format!(
                    "{} evaluated, {} cached",
                    t.prompt_n.unwrap_or(0),
                    t.cache_n.unwrap_or(0)
                ),
            ));
        }
    }

    let messages = vec![
        system,
        Message::user(vec![image_part, Part::Text(transcript.text.clone())]),
    ];
    let started = Instant::now();
    let mut ttft = None;
    let mut first_chunk = None;
    let mut chunker = SentenceChunker::new(ChunkerConfig::default());
    let mut spoken: u64 = 0;
    let mut say = |chunks: Vec<String>, first_chunk: &mut Option<(Duration, Duration)>| {
        for chunk in chunks {
            first_chunk.get_or_insert_with(|| (started.elapsed(), key_up.elapsed()));
            if let Some((voice, _)) = &speech
                && voice.speak(&chunk).is_ok()
            {
                spoken += 1;
            }
        }
    };
    print!("glance: ");
    let summary = complete(&client, messages, args.max_tokens, |text| {
        ttft.get_or_insert_with(|| (started.elapsed(), key_up.elapsed()));
        say(chunker.push(text), &mut first_chunk);
        print!("{text}");
        let _ = std::io::stdout().flush();
    })
    .await?;
    say(chunker.finish().into_iter().collect(), &mut first_chunk);
    println!();
    let generation = started.elapsed();

    // Wait for the last sentence to finish sounding.
    let mut first_audio = None;
    if let Some((_voice, progress)) = &speech {
        let mut progress = progress.clone();
        let finished = progress.wait_for(|p| p.ended >= spoken);
        match tokio::time::timeout(Duration::from_secs(120), finished).await {
            Ok(Ok(seen)) => first_audio = seen.first_audio,
            _ => eprintln!("warning: speech did not report finishing"),
        }
    }
    let speech_done = key_up.elapsed();

    if let Some((since_request, _)) = ttft {
        stage(&mut report, "llm time to first token", since_request);
    }
    if let Some((since_request, _)) = first_chunk {
        stage(&mut report, "llm first speakable chunk", since_request);
    }
    stage(&mut report, "llm whole reply", generation);
    if let Some(t) = &summary.timings {
        report.push((
            "  prompt tokens",
            format!(
                "{} evaluated, {} cached",
                t.prompt_n.unwrap_or(0),
                t.cache_n.unwrap_or(0)
            ),
        ));
        if let Some(rate) = t.predicted_per_second {
            report.push(("  generation speed", format!("{rate:>8.1} tok/s")));
        }
    }

    println!("\n--- per-stage timing ---");
    for (name, value) in &report {
        println!("{name:<32} {value}");
    }
    println!("--- from key-up ---");
    if let Some((_, since_key_up)) = ttft {
        println!("{:<32} {:>8.1} ms", "first token", ms(since_key_up));
    }
    if let Some((_, since_key_up)) = first_chunk {
        println!(
            "{:<32} {:>8.1} ms",
            "first chunk handed to TTS",
            ms(since_key_up)
        );
    }
    if speech.is_some() {
        match first_audio {
            Some(at) => println!(
                "{:<32} {:>8.1} ms   <- the metric that matters",
                "TIME TO FIRST AUDIO",
                ms(at.duration_since(key_up))
            ),
            None => println!("{:<32} never reported", "TIME TO FIRST AUDIO"),
        }
        println!(
            "{:<32} {:>8.1} ms   ({spoken} sentence(s))",
            "speech finished",
            ms(speech_done)
        );
    }
    Ok(())
}

/// Run one request to completion, handing each content delta to `on_text`.
pub async fn complete(
    client: &OpenAiClient,
    messages: Vec<Message>,
    max_tokens: u32,
    mut on_text: impl FnMut(&str),
) -> Result<ChatSummary> {
    let mut request = ChatRequest {
        messages,
        max_tokens: Some(max_tokens),
        temperature: Some(0.0),
        ..Default::default()
    };
    request
        .extra_body
        .insert("cache_prompt".into(), json!(true));
    let mut stream = client.chat(request).await?;
    while let Some(event) = stream.next().await {
        match event? {
            ChatEvent::Content(text) => on_text(&text),
            ChatEvent::Reasoning(_) => {}
            ChatEvent::Done(summary) => return Ok(summary),
        }
    }
    bail!("stream ended without a summary")
}

/// Rough "a sentence just finished" test for timing purposes. The real
/// sentence chunker arrives with TTS in M1.
pub fn ends_sentence(text: &str) -> bool {
    let trimmed = text.trim_end();
    trimmed.len() > 12 && trimmed.ends_with(['.', '!', '?'])
}

#[cfg(test)]
mod tests {
    use super::ends_sentence;

    #[test]
    fn sentence_end_detection() {
        assert!(ends_sentence("This is a code editor. "));
        assert!(ends_sentence("Is that what you meant?"));
        assert!(!ends_sentence("This is a code editor, and"));
        assert!(!ends_sentence("Yes."));
    }
}
