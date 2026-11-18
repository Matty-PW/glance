//! `glance fast-bench`: FAST-only comparison (the default setup: one small
//! model, no DEEP). For each model, on 6 hard and 6 ordinary questions (one
//! of each per eval scene):
//!
//! - fresh capture: the image is prewarmed at "key-down", the key is held
//!   for `--hold-ms` while the user would be talking, then the question goes
//!   out after speech recognition (0.12 s, M0) and the prewarm;
//! - warm: the same question as a follow-up on a screenshot already in the
//!   session (its image prefix is cached).
//!
//! Time to first word runs from key-up to the first sentence sounding
//! (Kokoro at volume 0, so the device runs but nothing is heard). Every
//! question is asked both ways: pass A asks the ordinary question fresh and
//! the hard one as its follow-up, pass B the other way round, on a restarted
//! server so no image is in llama-server's RAM cache.
//!
//! Memory: llama-server's resident size with its context allocated, and
//! glance's own with Parakeet and Kokoro loaded.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use glance_capture::{PngFile, ScreenCapture};
use glance_llm_client::{ChatEvent, ChatModel, Message, OpenAiClient, Part};
use glance_orchestrator::length::ends_sentence;
use glance_orchestrator::{ResponseLength, SPOKEN_SYSTEM_PROMPT};
use glance_stt::Parakeet;
use glance_tts::{ChunkerConfig, SentenceChunker};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::deep_bench::{STT_ESTIMATE, first_sentence, req, rss_mb, run_to_end, start_server};
use super::speech::{self, SpeechArgs};
use super::tts_bench::first_audio;
use super::{find_model, manifest, ms, store};

#[derive(clap::Args)]
pub struct Args {
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "qwen3-vl-4b-instruct-q4km,qwen3-vl-8b-instruct-q4km"
    )]
    models: Vec<String>,
    #[arg(long, default_value = "eval/set.toml")]
    set: PathBuf,
    #[arg(long, default_value = "eval-local/shots")]
    shots: PathBuf,
    #[arg(long, default_value_t = 1024)]
    max_edge: u32,
    #[arg(long, default_value_t = 8192)]
    ctx: u32,
    /// The app's reply budget.
    #[arg(long, default_value_t = 200)]
    max_tokens: u32,
    /// How long the key is held (the user talking) on a fresh capture.
    #[arg(long, default_value_t = 3000)]
    hold_ms: u64,
    /// response_length: short, medium (the default) or long.
    #[arg(long, default_value = "medium")]
    length: String,
    /// JSON with every answer and timing; a side-by-side .md is written next
    /// to it. Keep it in the gitignored eval-local/.
    #[arg(long, default_value = "eval-local/results/fast-compare.json")]
    out: PathBuf,
}

#[derive(Deserialize)]
struct Set {
    scene: Vec<Scene>,
}

#[derive(Deserialize)]
struct Scene {
    name: String,
    q: Vec<Question>,
}

#[derive(Deserialize, Clone)]
struct Question {
    ask: String,
    #[serde(default)]
    expect: Vec<Vec<String>>,
    #[serde(default)]
    hard: bool,
}

#[derive(Serialize, Clone)]
struct Turn {
    model: String,
    scene: String,
    kind: &'static str,
    condition: &'static str,
    question: String,
    /// Key-up to the question going out (STT plus any prewarm left).
    send_ms: f64,
    prewarm_ms: Option<f64>,
    first_token_ms: Option<f64>,
    first_sentence_ms: Option<f64>,
    tts_ms: Option<f64>,
    /// Key-up to the first sentence sounding.
    first_word_ms: Option<f64>,
    cache_n: Option<u64>,
    prompt_n: Option<u64>,
    tok_s: Option<f64>,
    finish_reason: Option<String>,
    /// Speech stopped at the response-length cap.
    capped: bool,
    words: usize,
    /// Ordinary questions only: all expected phrases present.
    correct: Option<bool>,
    answer: String,
}

#[derive(Serialize)]
struct ModelSummary {
    model: String,
    server_load_s: f64,
    server_rss_mb: u64,
    glance_rss_mb: u64,
}

pub async fn run(args: Args) -> Result<()> {
    let set: Set = toml::from_str(
        &std::fs::read_to_string(&args.set).with_context(|| args.set.display().to_string())?,
    )?;
    // One ordinary (the first) and the hard question per scene.
    let pairs: Vec<(String, Question, Question)> = set
        .scene
        .iter()
        .filter_map(|s| {
            let plain = s.q.iter().find(|q| !q.hard)?.clone();
            let hard = s.q.iter().find(|q| q.hard)?.clone();
            Some((s.name.clone(), plain, hard))
        })
        .collect();

    // glance's own memory: speech recognition and the voice, as in the app.
    let before = rss_mb(std::process::id());
    let stt = tokio::task::spawn_blocking(|| -> Result<Parakeet> {
        let manifest = manifest()?;
        let model = find_model(&manifest, "parakeet-tdt-0.6b-v3-int8")?;
        Ok(Parakeet::load(&store()?.verified_dir(model)?)?)
    })
    .await??;
    let speech_settings = SpeechArgs {
        volume: Some(0.0),
        ..Default::default()
    }
    .settings()?;
    let mut voice = tokio::task::spawn_blocking(move || speech::start(&speech_settings)).await?;
    let glance_rss = rss_mb(std::process::id());
    println!(
        "glance with STT + {}: {} MB resident ({} MB before loading them)",
        voice.description, glance_rss, before
    );

    super::clean_stale_servers();
    let mut turns: Vec<Turn> = Vec::new();
    let mut summaries = Vec::new();
    for model in &args.models {
        let mut server_rss = 0;
        let mut load = Duration::ZERO;
        for pass in ["A", "B"] {
            // A fresh server per pass: nothing in its RAM prompt cache.
            let started = Instant::now();
            let server = start_server(model, args.ctx, Vec::new()).await?;
            load = load.max(started.elapsed());
            let client = OpenAiClient::new(server.endpoint().clone())?;
            println!(
                "\n== {model}, pass {pass} (server up in {:.1} s)",
                started.elapsed().as_secs_f64()
            );
            for (scene, plain, hard) in &pairs {
                let (first, second) = if pass == "A" {
                    (plain, hard)
                } else {
                    (hard, plain)
                };
                let shot = PngFile::new(args.shots.join(format!("{scene}.png"))).capture()?;
                let key_down = Instant::now();
                let image = Part::Png(shot.encode(args.max_edge)?.png());
                let fresh = ask(
                    &client,
                    &args,
                    model,
                    scene,
                    first,
                    "fresh",
                    Vec::new(),
                    Some((image.clone(), key_down)),
                    &mut voice,
                )
                .await?;
                let history = vec![
                    Message::user(vec![image, Part::Text(first.ask.clone())]),
                    Message::assistant(fresh.answer.trim()),
                ];
                let warm = ask(
                    &client, &args, model, scene, second, "warm", history, None, &mut voice,
                )
                .await?;
                for t in [&fresh, &warm] {
                    println!(
                        "  {scene:<9} {:<5} {:<8} first word {:>6} (send {:>5.0} ms, first token {:>5}, cache {:>4}){}",
                        t.condition,
                        t.kind,
                        fmt_ms(t.first_word_ms),
                        t.send_ms,
                        fmt_ms(t.first_token_ms),
                        t.cache_n.unwrap_or(0),
                        match t.correct {
                            Some(true) => " correct",
                            Some(false) => " WRONG",
                            None => "",
                        }
                    );
                }
                turns.push(fresh);
                turns.push(warm);
            }
            server_rss = server_rss.max(rss_mb(server.pid()));
            drop(server);
        }
        summaries.push(ModelSummary {
            model: model.clone(),
            server_load_s: load.as_secs_f64(),
            server_rss_mb: server_rss,
            glance_rss_mb: glance_rss,
        });
    }
    drop(stt);

    println!("\n== summary (median / max over 12 questions each)");
    println!(
        "{:<28} {:>10} {:>12} {:>16} {:>16} {:>9} {:>9}",
        "model",
        "server MB",
        "+glance MB",
        "fresh first word",
        "warm first word",
        "correct",
        "cut off"
    );
    for s in &summaries {
        let of = |condition: &str| -> Vec<f64> {
            let mut v: Vec<f64> = turns
                .iter()
                .filter(|t| t.model == s.model && t.condition == condition)
                .filter_map(|t| t.first_word_ms)
                .collect();
            v.sort_by(f64::total_cmp);
            v
        };
        let stat = |v: Vec<f64>| match v.len() {
            0 => "-".to_owned(),
            n => format!("{:.2} / {:.2} s", v[n / 2] / 1000.0, v[n - 1] / 1000.0),
        };
        let mine: Vec<&Turn> = turns.iter().filter(|t| t.model == s.model).collect();
        let scored: Vec<&&Turn> = mine.iter().filter(|t| t.correct.is_some()).collect();
        let right = scored.iter().filter(|t| t.correct == Some(true)).count();
        let cut = mine
            .iter()
            .filter(|t| t.finish_reason.as_deref() == Some("length"))
            .count();
        println!(
            "{:<28} {:>10} {:>12} {:>16} {:>16} {:>9} {:>9}",
            s.model,
            s.server_rss_mb,
            s.server_rss_mb + s.glance_rss_mb,
            stat(of("fresh")),
            stat(of("warm")),
            format!("{right}/{}", scored.len()),
            format!("{cut}/{}", mine.len()),
        );
    }

    if let Some(dir) = args.out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        &args.out,
        serde_json::to_string_pretty(
            &json!({"models": summaries, "turns": turns, "hold_ms": args.hold_ms}),
        )?,
    )?;
    let md = args.out.with_extension("md");
    std::fs::write(&md, side_by_side(&args.models, &pairs, &turns))?;
    println!("answers: {} and {}", args.out.display(), md.display());
    Ok(())
}

/// One question; with `fresh`, prewarm the image at key-down first.
#[allow(clippy::too_many_arguments)]
async fn ask(
    client: &OpenAiClient,
    args: &Args,
    model: &str,
    scene: &str,
    question: &Question,
    condition: &'static str,
    history: Vec<Message>,
    fresh: Option<(Part, Instant)>,
    voice: &mut speech::Speech,
) -> Result<Turn> {
    let body: Value =
        json!({"cache_prompt": true, "chat_template_kwargs": {"enable_thinking": false}});
    let length = ResponseLength::parse(&args.length).context("--length: short, medium or long")?;
    let mut prompt = SPOKEN_SYSTEM_PROMPT.to_owned();
    if let Some(hint) = length.hint(false) {
        prompt.push_str(hint);
    }
    let system = Message::system(prompt);
    let (image, key_up, prewarm) = match fresh {
        Some((image, key_down)) => {
            let warm_request = req(
                vec![system.clone(), Message::user(vec![image.clone()])],
                1,
                &body,
                false,
            );
            let client2 = client.clone();
            let prewarm = tokio::spawn(async move {
                let t = Instant::now();
                run_to_end(&client2, warm_request)
                    .await
                    .map(|_| t.elapsed())
            });
            // The user talks while the image is processed.
            tokio::time::sleep_until((key_down + Duration::from_millis(args.hold_ms)).into()).await;
            (Some(image), Instant::now(), Some(prewarm))
        }
        None => (None, Instant::now(), None),
    };
    tokio::time::sleep(STT_ESTIMATE).await;
    let prewarm_ms = match prewarm {
        Some(task) => Some(ms(task.await??)),
        None => None,
    };
    let mut parts = Vec::new();
    if let Some(image) = image {
        parts.push(image);
    }
    parts.push(Part::Text(question.ask.clone()));
    let mut messages = vec![system];
    messages.extend(history);
    messages.push(Message::user(parts));

    let send = key_up.elapsed();
    let sent_at = Instant::now();
    let mut stream = client
        .chat(req(
            messages,
            length.max_tokens(args.max_tokens, &question.ask),
            &body,
            false,
        ))
        .await?;
    let mut chunker = SentenceChunker::new(ChunkerConfig::default());
    let (mut first_token, mut first_chunk) = (None, None);
    let mut answer = String::new();
    let mut summary = None;
    // As the app does: stop at the sentence cap and keep what was spoken.
    let cap = length.sentence_cap(&question.ask);
    let (mut spoken, mut sentences, mut capped) = (String::new(), 0, false);
    'read: while let Some(event) = stream.next().await {
        match event? {
            ChatEvent::Content(delta) => {
                first_token.get_or_insert_with(|| sent_at.elapsed());
                answer.push_str(&delta);
                for chunk in chunker.push(&delta) {
                    first_chunk.get_or_insert_with(|| sent_at.elapsed());
                    spoken.push_str(&chunk);
                    spoken.push(' ');
                    if ends_sentence(&chunk) {
                        sentences += 1;
                        if cap == Some(sentences) {
                            capped = true;
                            break 'read;
                        }
                    }
                }
            }
            ChatEvent::Reasoning(_) => {}
            ChatEvent::Done(s) => {
                if let Some(rest) = chunker.finish() {
                    first_chunk.get_or_insert_with(|| sent_at.elapsed());
                    spoken.push_str(&rest);
                }
                summary = Some(s);
                break;
            }
        }
    }
    drop(stream);
    let answer = spoken.trim().to_owned();
    let tts = match first_sentence(&answer) {
        // A speech failure is recorded (no first word), not fatal.
        Some(sentence) => match first_audio(&*voice.tts, &mut voice.events, &sentence).await {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("  speech failed: {e:#}");
                None
            }
        },
        None => None,
    };
    let first_word = first_chunk.zip(tts).map(|(c, t)| send + c + t);
    let timings = summary.as_ref().and_then(|s| s.timings.clone());
    let correct = (!question.hard && !question.expect.is_empty()).then(|| {
        let lower = answer.to_lowercase();
        question
            .expect
            .iter()
            .all(|group| group.iter().any(|alt| lower.contains(&alt.to_lowercase())))
    });
    Ok(Turn {
        model: model.to_owned(),
        scene: scene.to_owned(),
        kind: if question.hard { "hard" } else { "ordinary" },
        condition,
        question: question.ask.clone(),
        send_ms: ms(send),
        prewarm_ms,
        first_token_ms: first_token.map(ms),
        first_sentence_ms: first_chunk.map(ms),
        tts_ms: tts.map(ms),
        first_word_ms: first_word.map(ms),
        cache_n: timings.as_ref().and_then(|t| t.cache_n),
        prompt_n: timings.as_ref().and_then(|t| t.prompt_n),
        tok_s: timings.as_ref().and_then(|t| t.predicted_per_second),
        finish_reason: summary.and_then(|s| s.finish_reason),
        capped,
        words: answer.split_whitespace().count(),
        correct,
        answer,
    })
}

/// Markdown for reading the models' answers next to each other.
fn side_by_side(
    models: &[String],
    pairs: &[(String, Question, Question)],
    turns: &[Turn],
) -> String {
    let mut md = String::from("# FAST comparison: answers side by side\n\n");
    md.push_str("Fixed eval questions; answers are model output for review only.\n");
    for (scene, plain, hard) in pairs {
        for q in [plain, hard] {
            md.push_str(&format!(
                "\n## {scene} ({}): {}\n",
                if q.hard { "hard" } else { "ordinary" },
                q.ask
            ));
            for model in models {
                for condition in ["fresh", "warm"] {
                    if let Some(t) = turns.iter().find(|t| {
                        &t.model == model
                            && t.scene == *scene
                            && t.question == q.ask
                            && t.condition == condition
                    }) {
                        let flag = match t.correct {
                            Some(true) => " ✓",
                            Some(false) => " ✗",
                            None => "",
                        };
                        md.push_str(&format!(
                            "\n**{model}**, {condition}{flag} ({}):\n\n> {}\n",
                            fmt_ms(t.first_word_ms),
                            t.answer.trim().replace('\n', "\n> ")
                        ));
                    }
                }
            }
        }
    }
    md
}

fn fmt_ms(value: Option<f64>) -> String {
    value.map_or("-".into(), |v| format!("{:.2} s", v / 1000.0))
}
