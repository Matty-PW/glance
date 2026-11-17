//! `glance deep-bench`: the DEEP bake-off (`docs/DESIGN.md` §7.2, M4).
//!
//! Starts FAST and DEEP as managed llama-servers, side by side as in the app,
//! then for each `hard` question of the eval set:
//! 1. FAST, with the escalation prompt and grammar: time to its decision.
//! 2. For each reasoning effort: DEEP prewarm (image only, same effort), then
//!    the question. Three timings, measured separately:
//!    - thinking: first thinking token to first answer token,
//!    - answer: first answer token to the end,
//!    - first spoken word: DEEP request to its first sentence sounding
//!      (Kokoro at volume 0, so the device runs but nothing is heard).
//!
//! The estimate of key-up to DEEP's first word adds speech recognition
//! (0.12 s, M0) and FAST's decision time. Answers go to `--out` (keep it in
//! the gitignored eval-local/) for the maintainer's quality spot-check.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use glance_capture::{PngFile, ScreenCapture};
use glance_llm_client::escalate::{Decision, EscalationGate, GRAMMAR, Step};
use glance_llm_client::server::{ManagedServer, ServerSpec};
use glance_llm_client::{
    ChatEvent, ChatModel, ChatRequest, ChatSummary, Message, OpenAiClient, Part,
};
use glance_models::Kind;
use glance_orchestrator::{DEEP_SYSTEM_PROMPT, ESCALATION_SYSTEM_PROMPT};
use glance_tts::{ChunkerConfig, SentenceChunker};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::speech::{self, SpeechArgs};
use super::tts_bench::first_audio;
use super::{find_model, manifest, ms, store};

/// Speech recognition of a typical question (M0: 116 ms for 2.7 s).
const STT_ESTIMATE: Duration = Duration::from_millis(120);

#[derive(clap::Args)]
pub struct Args {
    #[arg(long, default_value = "qwen3-vl-4b-instruct-q4km")]
    fast: String,
    #[arg(long, default_value = "qwen3.8-27b-q4km")]
    deep: String,
    /// Reasoning efforts to compare (Qwen3.8: low, medium, xhigh).
    #[arg(long, value_delimiter = ',', default_value = "low,medium,xhigh")]
    efforts: Vec<String>,
    #[arg(long, default_value = "eval/set.toml")]
    set: PathBuf,
    #[arg(long, default_value = "eval-local/shots")]
    shots: PathBuf,
    #[arg(long, default_value_t = 1024)]
    fast_edge: u32,
    #[arg(long, default_value_t = 2048)]
    deep_edge: u32,
    #[arg(long, default_value_t = 16384)]
    deep_ctx: u32,
    /// DEEP's budget, thinking included.
    #[arg(long, default_value_t = 4096)]
    deep_max_tokens: u32,
    /// Run DEEP without its built-in MTP draft (speculative decoding).
    #[arg(long)]
    no_mtp: bool,
    /// Extra llama-server flag for DEEP (repeatable), e.g.
    /// --deep-arg=--checkpoint-min-step --deep-arg=64.
    #[arg(long = "deep-arg", allow_hyphen_values = true)]
    deep_args: Vec<String>,
    /// Only the first N hard questions.
    #[arg(long)]
    limit: Option<usize>,
    /// Write every answer and timing here as JSON.
    #[arg(long)]
    out: Option<PathBuf>,
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

#[derive(Deserialize)]
struct Question {
    ask: String,
    #[serde(default)]
    hard: bool,
}

#[derive(Serialize, Default, Clone)]
struct DeepRun {
    scene: String,
    question: String,
    effort: String,
    prewarm_ms: f64,
    prewarm_prompt_n: Option<u64>,
    /// Request to first thinking token (prompt evaluation of the question).
    to_thinking_ms: Option<f64>,
    thinking_ms: Option<f64>,
    /// Request to first answer token.
    to_answer_ms: Option<f64>,
    answer_ms: Option<f64>,
    to_first_sentence_ms: Option<f64>,
    tts_first_audio_ms: Option<f64>,
    /// DEEP request to its first sound.
    first_word_ms: Option<f64>,
    /// Estimated key-up to DEEP's first sound.
    keyup_first_word_ms: Option<f64>,
    cache_n: Option<u64>,
    prompt_n: Option<u64>,
    predicted_n: Option<u64>,
    tok_s: Option<f64>,
    draft_n: Option<u64>,
    draft_accepted: Option<u64>,
    thinking_chars: usize,
    finish_reason: Option<String>,
    answer: String,
}

#[derive(Serialize)]
struct FastRun {
    scene: String,
    decision: String,
    decision_ms: f64,
}

pub async fn run(args: Args) -> Result<()> {
    let set: Set = toml::from_str(
        &std::fs::read_to_string(&args.set).with_context(|| args.set.display().to_string())?,
    )?;
    let mut questions: Vec<(String, String)> = set
        .scene
        .iter()
        .flat_map(|s| {
            s.q.iter()
                .filter(|q| q.hard)
                .map(|q| (s.name.clone(), q.ask.clone()))
        })
        .collect();
    if let Some(n) = args.limit {
        questions.truncate(n);
    }
    for effort in &args.efforts {
        if !["low", "medium", "xhigh"].contains(&effort.as_str()) {
            bail!("effort `{effort}`: Qwen3.8 accepts low, medium, xhigh");
        }
    }

    // Both servers at once, as the app runs them.
    let mut mtp = if args.no_mtp {
        Vec::new()
    } else {
        vec!["--spec-type".into(), "draft-mtp".into()]
    };
    mtp.extend(args.deep_args.iter().cloned());
    if !args.deep_args.is_empty() {
        println!("DEEP extra flags: {}", args.deep_args.join(" "));
    }
    let started = Instant::now();
    let (fast, deep) = tokio::try_join!(
        start_server(&args.fast, 8192, Vec::new()),
        start_server(&args.deep, args.deep_ctx, mtp),
    )?;
    println!(
        "servers up in {:.1} s (FAST {} MB, DEEP {} MB resident); MTP {}",
        started.elapsed().as_secs_f64(),
        rss_mb(fast.pid()),
        rss_mb(deep.pid()),
        if args.no_mtp { "off" } else { "on" }
    );
    let fast_client = OpenAiClient::new(fast.endpoint().clone())?;
    let deep_client = OpenAiClient::new(deep.endpoint().clone())?;

    let speech_settings = SpeechArgs {
        volume: Some(0.0),
        ..Default::default()
    }
    .settings()?;
    let mut voice = tokio::task::spawn_blocking(move || speech::start(&speech_settings)).await?;
    println!("speech: {} (volume 0)", voice.description);

    let mut fast_runs = Vec::new();
    let mut deep_runs: Vec<DeepRun> = Vec::new();
    for (scene, ask) in &questions {
        let shot = PngFile::new(args.shots.join(format!("{scene}.png"))).capture()?;
        let fast_image = Part::Png(shot.encode(args.fast_edge)?.png());
        let deep_image = Part::Png(shot.encode(args.deep_edge)?.png());

        // FAST: prewarm, then the question with the escalation option.
        let fast_body =
            json!({"cache_prompt": true, "chat_template_kwargs": {"enable_thinking": false}});
        let system = Message::system(ESCALATION_SYSTEM_PROMPT);
        run_to_end(
            &fast_client,
            req(
                vec![system.clone(), Message::user(vec![fast_image.clone()])],
                1,
                &fast_body,
                false,
            ),
        )
        .await?;
        let t = Instant::now();
        let mut stream = fast_client
            .chat(req(
                vec![
                    system,
                    Message::user(vec![fast_image, Part::Text(ask.clone())]),
                ],
                200,
                &fast_body,
                true,
            ))
            .await?;
        let mut gate = EscalationGate::new();
        let mut decision = None;
        while let Some(event) = stream.next().await {
            match event? {
                ChatEvent::Content(delta) => {
                    if let Step::Stop(d) = gate.push(&delta) {
                        decision = Some(d);
                        break;
                    }
                    if gate.decision() == Some(Decision::Answer) {
                        break;
                    }
                }
                ChatEvent::Done(_) => break,
                ChatEvent::Reasoning(_) => {}
            }
        }
        drop(stream);
        let decision = decision.or(gate.decision()).unwrap_or(Decision::Answer);
        let decision_time = t.elapsed();
        println!(
            "\n[{scene}] FAST decided {decision:?} in {:.0} ms",
            ms(decision_time)
        );
        fast_runs.push(FastRun {
            scene: scene.clone(),
            decision: format!("{decision:?}"),
            decision_ms: ms(decision_time),
        });

        for effort in &args.efforts {
            let body = json!({
                "cache_prompt": true,
                "chat_template_kwargs": {"reasoning_effort": effort},
            });
            let system = Message::system(DEEP_SYSTEM_PROMPT);
            let t = Instant::now();
            let warm = run_to_end(
                &deep_client,
                req(
                    vec![system.clone(), Message::user(vec![deep_image.clone()])],
                    1,
                    &body,
                    false,
                ),
            )
            .await?;
            let prewarm = t.elapsed();
            let mut run = deep_once(
                &deep_client,
                req(
                    vec![
                        system,
                        Message::user(vec![deep_image.clone(), Part::Text(ask.clone())]),
                    ],
                    args.deep_max_tokens,
                    &body,
                    false,
                ),
            )
            .await?;
            run.scene = scene.clone();
            run.question = ask.clone();
            run.effort = effort.clone();
            run.prewarm_ms = ms(prewarm);
            run.prewarm_prompt_n = warm.timings.and_then(|t| t.prompt_n);
            // Speak DEEP's first sentence to time the voice for real.
            if let Some(sentence) = first_sentence(&run.answer) {
                let audio = first_audio(&*voice.tts, &mut voice.events, &sentence).await?;
                run.tts_first_audio_ms = Some(ms(audio));
                run.first_word_ms = run.to_first_sentence_ms.map(|t| t + ms(audio));
                run.keyup_first_word_ms = run
                    .first_word_ms
                    .map(|t| t + ms(STT_ESTIMATE) + ms(decision_time));
            }
            println!(
                "  {effort:<6} prewarm {:>6.0} ms | thinking {:>7} | answer {:>7} | first word {:>7} (key-up est. {:>7}) | {} tok, {} tok/s, draft {}/{}, cache {}",
                run.prewarm_ms,
                fmt_s(run.thinking_ms),
                fmt_s(run.answer_ms),
                fmt_s(run.first_word_ms),
                fmt_s(run.keyup_first_word_ms),
                run.predicted_n.unwrap_or(0),
                run.tok_s.map_or("-".into(), |r| format!("{r:.1}")),
                run.draft_accepted.unwrap_or(0),
                run.draft_n.unwrap_or(0),
                run.cache_n.unwrap_or(0),
            );
            deep_runs.push(run);
        }
    }

    println!(
        "\n== summary per effort (median / max over {} questions)",
        questions.len()
    );
    println!(
        "{:<7} {:>16} {:>16} {:>18} {:>22}",
        "effort", "thinking s", "answer s", "first word s", "key-up→first word s"
    );
    for effort in &args.efforts {
        let runs: Vec<&DeepRun> = deep_runs.iter().filter(|r| &r.effort == effort).collect();
        let stat = |f: fn(&DeepRun) -> Option<f64>| {
            let mut v: Vec<f64> = runs.iter().filter_map(|r| f(r)).collect();
            v.sort_by(f64::total_cmp);
            match v.len() {
                0 => "-".to_owned(),
                n => format!("{:.1} / {:.1}", v[n / 2] / 1000.0, v[n - 1] / 1000.0),
            }
        };
        println!(
            "{:<7} {:>16} {:>16} {:>18} {:>22}",
            effort,
            stat(|r| r.thinking_ms),
            stat(|r| r.answer_ms),
            stat(|r| r.first_word_ms),
            stat(|r| r.keyup_first_word_ms)
        );
    }
    println!(
        "Budget (DESIGN §12): filler within 1.1 s of key-up, DEEP's first audio within 4 s of the \
         filler when prewarmed, so about 5 s from key-up."
    );

    if let Some(out) = &args.out {
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = json!({"fast": fast_runs, "deep": deep_runs, "mtp": !args.no_mtp});
        std::fs::write(out, serde_json::to_string_pretty(&json)?)?;
        println!("answers and timings written to {}", out.display());
    }
    drop((fast, deep));
    Ok(())
}

fn req(messages: Vec<Message>, max_tokens: u32, body: &Value, grammar: bool) -> ChatRequest {
    let mut request = ChatRequest {
        messages,
        max_tokens: Some(max_tokens),
        temperature: Some(0.0),
        ..Default::default()
    };
    if let Some(map) = body.as_object() {
        request.extra_body = map.clone();
    }
    if grammar {
        request.extra_body.insert("grammar".into(), json!(GRAMMAR));
    }
    request
}

async fn run_to_end(client: &OpenAiClient, request: ChatRequest) -> Result<ChatSummary> {
    let mut stream = client.chat(request).await?;
    while let Some(event) = stream.next().await {
        if let ChatEvent::Done(summary) = event? {
            return Ok(summary);
        }
    }
    bail!("stream ended without a summary")
}

/// One DEEP question, timing thinking and answer separately.
async fn deep_once(client: &OpenAiClient, request: ChatRequest) -> Result<DeepRun> {
    let started = Instant::now();
    let mut stream = client.chat(request).await?;
    let mut run = DeepRun::default();
    let (mut first_thinking, mut first_answer, mut first_sentence) = (None, None, None);
    let mut chunker = SentenceChunker::new(ChunkerConfig::default());
    while let Some(event) = stream.next().await {
        match event? {
            ChatEvent::Reasoning(delta) => {
                first_thinking.get_or_insert_with(|| started.elapsed());
                run.thinking_chars += delta.chars().count();
            }
            ChatEvent::Content(delta) => {
                first_answer.get_or_insert_with(|| started.elapsed());
                run.answer.push_str(&delta);
                if first_sentence.is_none() && !chunker.push(&delta).is_empty() {
                    first_sentence = Some(started.elapsed());
                }
            }
            ChatEvent::Done(summary) => {
                if first_sentence.is_none() && chunker.finish().is_some() {
                    first_sentence = Some(started.elapsed());
                }
                if let Some(t) = summary.timings {
                    run.cache_n = t.cache_n;
                    run.prompt_n = t.prompt_n;
                    run.predicted_n = t.predicted_n;
                    run.tok_s = t.predicted_per_second;
                    run.draft_n = t.draft_n;
                    run.draft_accepted = t.draft_n_accepted;
                }
                run.finish_reason = summary.finish_reason;
                break;
            }
        }
    }
    let end = started.elapsed();
    run.to_thinking_ms = first_thinking.map(ms);
    run.to_answer_ms = first_answer.map(ms);
    run.thinking_ms = match (first_thinking, first_answer) {
        (Some(a), Some(b)) => Some(ms(b.saturating_sub(a))),
        (Some(a), None) => Some(ms(end.saturating_sub(a))),
        _ => None,
    };
    run.answer_ms = first_answer.map(|a| ms(end.saturating_sub(a)));
    run.to_first_sentence_ms = first_sentence.map(ms);
    Ok(run)
}

fn first_sentence(answer: &str) -> Option<String> {
    let mut chunker = SentenceChunker::new(ChunkerConfig::default());
    let mut chunks = chunker.push(answer);
    chunks.extend(chunker.finish());
    chunks.into_iter().next()
}

async fn start_server(id: &str, ctx: u32, extra_args: Vec<String>) -> Result<ManagedServer> {
    let id = id.to_owned();
    let spec = tokio::task::spawn_blocking(move || -> Result<ServerSpec> {
        let manifest = manifest()?;
        let model = find_model(&manifest, &id)?;
        let dir = store()?
            .verified_dir(model)
            .with_context(|| format!("run `glance models pull {id}` first"))?;
        let file = |kind: Kind| {
            model
                .files
                .iter()
                .find(|f| f.kind == kind)
                .map(|f| dir.join(&f.path))
                .with_context(|| format!("{id} has no {kind:?} file"))
        };
        Ok(ServerSpec {
            weights: file(Kind::Weights)?,
            mmproj: file(Kind::Mmproj)?,
            alias: id.clone(),
            ctx,
            show_output: false,
            start_timeout: Duration::from_secs(600),
            extra_args,
        })
    })
    .await??;
    Ok(ManagedServer::start(&spec).await?)
}

/// Resident memory of a process in MB (`ps`).
fn rss_mb(pid: u32) -> u64 {
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map_or(0, |kb| kb / 1024)
}

fn fmt_s(value: Option<f64>) -> String {
    value.map_or("-".into(), |v| format!("{:.1} s", v / 1000.0))
}
