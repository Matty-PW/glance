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
pub(super) const STT_ESTIMATE: Duration = Duration::from_millis(120);

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
    /// Start DEEP fresh before each question, as "think harder" does when
    /// DEEP is not loaded: measures load + image prefill + thinking.
    #[arg(long)]
    cold: bool,
    /// Prewarm as [system, user:image] without the assistant turn opened,
    /// and ask as [system, user:image, user:question], so the prewarm prompt
    /// is an exact prefix of the question's (hybrid models only restore
    /// cached state from the end of a previous prompt).
    #[arg(long)]
    split_prewarm: bool,
    /// Only the first N hard questions.
    #[arg(long)]
    limit: Option<usize>,
    /// Only these eval scenes (e.g. sheet,chart).
    #[arg(long, value_delimiter = ',')]
    scenes: Vec<String>,
    /// response_length: short, medium (the default) or long.
    #[arg(long, default_value = "medium")]
    length: String,
    /// Personalities to run in this order for each question and effort
    /// (prewarm, then the question), e.g. neutral,friendly,friendly: the
    /// prewarm token counts show when a switch re-prefills.
    #[arg(long, value_delimiter = ',', default_value = "neutral")]
    personalities: Vec<String>,
    /// Keep each answer as history for the next run on the same question,
    /// as follow-ups in a session do (with --split-prewarm): each prewarm
    /// then extends what DEEP has cached instead of going back to the image.
    #[arg(long)]
    session: bool,
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
    personality: String,
    /// `--cold`: starting DEEP's server until it is ready.
    load_ms: Option<f64>,
    /// `--cold`: a think-harder tap to DEEP's first sound (load, prefill,
    /// then the question to its first sentence sounding).
    cold_first_word_ms: Option<f64>,
    answer_words: usize,
    capped: bool,
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
    if !args.scenes.is_empty() {
        questions.retain(|(scene, _)| args.scenes.contains(scene));
    }
    let length = glance_orchestrator::ResponseLength::parse(&args.length)
        .context("--length: short, medium or long")?;
    if let Some(n) = args.limit {
        questions.truncate(n);
    }
    for effort in &args.efforts {
        if !["off", "low", "medium", "xhigh"].contains(&effort.as_str()) {
            bail!("effort `{effort}`: Qwen3.8 accepts low, medium, xhigh (or off: no thinking)");
        }
    }

    super::clean_stale_servers();
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
    let fast = start_server(&args.fast, 8192, Vec::new()).await?;
    let mut deep = if args.cold {
        None
    } else {
        Some(start_server(&args.deep, args.deep_ctx, mtp.clone()).await?)
    };
    println!(
        "servers up in {:.1} s (FAST {} MB, DEEP {}); MTP {}",
        started.elapsed().as_secs_f64(),
        rss_mb(fast.pid()),
        deep.as_ref()
            .map_or("started per question (--cold)".to_owned(), |d| format!(
                "{} MB resident",
                rss_mb(d.pid())
            )),
        if args.no_mtp { "off" } else { "on" }
    );
    let fast_client = OpenAiClient::new(fast.endpoint().clone())?;

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
        let mut load_ms = None;
        if args.cold {
            drop(deep.take()); // stops the previous server
            let t = Instant::now();
            let server = start_server(&args.deep, args.deep_ctx, mtp.clone()).await?;
            load_ms = Some(ms(t.elapsed()));
            println!(
                "\n[{scene}] DEEP loaded in {:.1} s ({} MB resident)",
                t.elapsed().as_secs_f64(),
                rss_mb(server.pid())
            );
            deep = Some(server);
        }
        let deep_client =
            OpenAiClient::new(deep.as_ref().expect("DEEP is running").endpoint().clone())?;
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

        let mut session: Vec<Message> = Vec::new();
        let runs: Vec<(String, String)> = args
            .personalities
            .iter()
            .flat_map(|p| args.efforts.iter().map(|e| (p.clone(), e.clone())))
            .collect();
        for (personality, effort) in &runs {
            let layer = glance_orchestrator::Personality::parse(personality, "")
                .map_err(anyhow::Error::msg)?
                .layer();
            let kwargs = if effort == "off" {
                json!({"enable_thinking": false})
            } else {
                json!({"reasoning_effort": effort})
            };
            let body = json!({"cache_prompt": true, "chat_template_kwargs": kwargs});
            let mut prompt = DEEP_SYSTEM_PROMPT.to_owned();
            if let Some(hint) = length.hint(true) {
                prompt.push_str(hint);
            }
            if let Some(layer) = &layer {
                prompt.push_str(layer);
            }
            let system = Message::system(prompt);
            let image_message = Message::user(vec![deep_image.clone()]);
            let mut warm_body = body.clone();
            if args.split_prewarm {
                warm_body["add_generation_prompt"] = json!(false);
            }
            let t = Instant::now();
            let warm = run_to_end(
                &deep_client,
                req(
                    [vec![system.clone(), image_message.clone()], session.clone()].concat(),
                    1,
                    &warm_body,
                    false,
                ),
            )
            .await?;
            let prewarm = t.elapsed();
            let question = if args.split_prewarm {
                [
                    vec![system, image_message],
                    session.clone(),
                    vec![Message::user_text(ask.clone())],
                ]
                .concat()
            } else {
                vec![
                    system,
                    Message::user(vec![deep_image.clone(), Part::Text(ask.clone())]),
                ]
            };
            let mut run = deep_once(
                &deep_client,
                req(
                    question,
                    length.max_tokens(args.deep_max_tokens, ask),
                    &body,
                    false,
                ),
                length.sentence_cap(ask),
            )
            .await?;
            run.scene = scene.clone();
            run.question = ask.clone();
            run.effort = effort.clone();
            run.personality = personality.clone();
            run.prewarm_ms = ms(prewarm);
            run.load_ms = load_ms.take();
            run.answer_words = run.answer.split_whitespace().count();
            run.prewarm_prompt_n = warm.timings.and_then(|t| t.prompt_n);
            // Speak DEEP's first sentence to time the voice for real.
            if let Some(sentence) = first_sentence(&run.answer) {
                let audio = first_audio(&*voice.tts, &mut voice.events, &sentence).await?;
                run.tts_first_audio_ms = Some(ms(audio));
                run.first_word_ms = run.to_first_sentence_ms.map(|t| t + ms(audio));
                run.keyup_first_word_ms = run
                    .first_word_ms
                    .map(|t| t + ms(STT_ESTIMATE) + ms(decision_time));
                if let Some(load) = run.load_ms {
                    run.cold_first_word_ms = run.first_word_ms.map(|t| load + run.prewarm_ms + t);
                }
            }
            println!(
                "  {effort:<6} {personality:<8} prewarm {:>6.0} ms ({} tok processed) | thinking {:>7} | answer {:>7} | first word {:>7} (key-up est. {:>7}) | {} tok, {} tok/s, draft {}/{}, cache {}",
                run.prewarm_ms,
                run.prewarm_prompt_n.unwrap_or(0),
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
            if args.session {
                session.push(Message::user_text(ask.clone()));
                session.push(Message::assistant(run.answer.clone()));
            }
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
    if args.cold {
        println!("\n== cold think harder (DEEP not loaded), median / max");
        for effort in &args.efforts {
            let runs: Vec<&DeepRun> = deep_runs.iter().filter(|r| &r.effort == effort).collect();
            let stat = |f: &dyn Fn(&DeepRun) -> Option<f64>| {
                let mut v: Vec<f64> = runs.iter().filter_map(|r| f(r)).collect();
                v.sort_by(f64::total_cmp);
                match v.len() {
                    0 => "-".to_owned(),
                    n => format!("{:.1} / {:.1} s", v[n / 2] / 1000.0, v[n - 1] / 1000.0),
                }
            };
            let mut words: Vec<usize> = runs.iter().map(|r| r.answer_words).collect();
            words.sort_unstable();
            println!(
                "{effort:<7} load {} | image prefill {} | thinking {} | tap -> first word {} | answer words median {}",
                stat(&|r| r.load_ms),
                stat(&|r| Some(r.prewarm_ms)),
                stat(&|r| r.thinking_ms),
                stat(&|r| r.cold_first_word_ms),
                words.get(words.len() / 2).copied().unwrap_or(0)
            );
        }
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

pub(super) fn req(
    messages: Vec<Message>,
    max_tokens: u32,
    body: &Value,
    grammar: bool,
) -> ChatRequest {
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

pub(super) async fn run_to_end(client: &OpenAiClient, request: ChatRequest) -> Result<ChatSummary> {
    let mut stream = client.chat(request).await?;
    while let Some(event) = stream.next().await {
        if let ChatEvent::Done(summary) = event? {
            return Ok(summary);
        }
    }
    bail!("stream ended without a summary")
}

/// One DEEP question, timing thinking and answer separately.
async fn deep_once(
    client: &OpenAiClient,
    request: ChatRequest,
    cap: Option<usize>,
) -> Result<DeepRun> {
    let started = Instant::now();
    let mut stream = client.chat(request).await?;
    let mut run = DeepRun::default();
    let (mut first_thinking, mut first_answer, mut first_sentence) = (None, None, None);
    let mut chunker = SentenceChunker::new(ChunkerConfig::default());
    let mut sentences = 0;
    while let Some(event) = stream.next().await {
        match event? {
            ChatEvent::Reasoning(delta) => {
                first_thinking.get_or_insert_with(|| started.elapsed());
                run.thinking_chars += delta.chars().count();
            }
            ChatEvent::Content(delta) => {
                first_answer.get_or_insert_with(|| started.elapsed());
                // As the app does: stop at the sentence cap, keep what was
                // spoken.
                let mut at_cap = false;
                for chunk in chunker.push(&delta) {
                    first_sentence.get_or_insert_with(|| started.elapsed());
                    run.answer.push_str(&chunk);
                    run.answer.push(' ');
                    if glance_orchestrator::length::ends_sentence(&chunk) {
                        sentences += 1;
                        at_cap = cap == Some(sentences);
                        if at_cap {
                            break;
                        }
                    }
                }
                if at_cap {
                    run.capped = true;
                    break;
                }
            }
            ChatEvent::Done(summary) => {
                if let Some(rest) = chunker.finish() {
                    first_sentence.get_or_insert_with(|| started.elapsed());
                    run.answer.push_str(&rest);
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

pub(super) fn first_sentence(answer: &str) -> Option<String> {
    let mut chunker = SentenceChunker::new(ChunkerConfig::default());
    let mut chunks = chunker.push(answer);
    chunks.extend(chunker.finish());
    chunks.into_iter().next()
}

pub(super) async fn start_server(
    id: &str,
    ctx: u32,
    extra_args: Vec<String>,
) -> Result<ManagedServer> {
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
            pid_dir: super::run_dir().ok(),
        })
    })
    .await??;
    Ok(ManagedServer::start(&spec).await?)
}

/// Resident memory of a process in MB (`ps`).
pub(super) fn rss_mb(pid: u32) -> u64 {
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
