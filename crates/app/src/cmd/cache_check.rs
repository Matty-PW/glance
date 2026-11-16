//! `glance cache-check`: does the server reuse a cached image prefix?
//!
//! The prewarm design (`docs/DESIGN.md` §4) sends the screenshot at key-down
//! and the question at key-up, and only works if the second request reuses the
//! image tokens the first one already processed. This command measures that
//! directly from llama-server's `timings.cache_n`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use glance_capture::{PngFile, ScreenCapture};
use glance_llm_client::{
    ChatEvent, ChatModel, ChatRequest, Endpoint, Message, OpenAiClient, Part, Timings,
};
use serde_json::json;

use super::{SPOKEN_SYSTEM_PROMPT, ms};

#[derive(clap::Args)]
pub struct Args {
    /// A real screenshot (PNG). It is read into memory and sent only to the
    /// endpoint below.
    #[arg(long)]
    png: PathBuf,
    /// OpenAI-compatible API root, e.g. http://127.0.0.1:8080/v1 (loopback only).
    #[arg(long)]
    endpoint: String,
    /// Model name to request.
    #[arg(long, default_value = "default")]
    model: String,
    /// Longest edge of the image sent to the model, in pixels.
    #[arg(long, default_value_t = 1024)]
    max_edge: u32,
    #[arg(long, default_value = "What app is this and what is it showing?")]
    question: String,
    #[arg(long, default_value = "What is the most important thing on screen?")]
    follow_up: String,
}

struct Outcome {
    label: &'static str,
    ttft: Option<Duration>,
    total: Duration,
    timings: Timings,
    reply: String,
}

impl Outcome {
    fn cached(&self) -> u64 {
        self.timings.cache_n.unwrap_or(0)
    }
    fn evaluated(&self) -> u64 {
        self.timings.prompt_n.unwrap_or(0)
    }
    fn prompt_tokens(&self) -> u64 {
        self.cached() + self.evaluated()
    }
}

pub async fn run(args: Args) -> Result<()> {
    let endpoint = Endpoint::loopback(&args.endpoint, &args.model)?;
    let client = OpenAiClient::new(endpoint)?;

    let started = Instant::now();
    let shot = PngFile::new(&args.png).capture()?;
    let decode = started.elapsed();
    let started = Instant::now();
    let image = shot.encode(args.max_edge)?;
    let encode = started.elapsed();
    println!(
        "image: {}x{} -> {}x{}, {} kB PNG (decode {} ms, downscale+encode {} ms)",
        shot.width(),
        shot.height(),
        image.width(),
        image.height(),
        image.len() / 1000,
        ms(decode),
        ms(encode)
    );

    let system = Message::system(SPOKEN_SYSTEM_PROMPT);
    let image_part = Part::Png(image.png());

    // 1. Prewarm: what glance sends at key-down. Image only, one token.
    let prewarm = ask(
        &client,
        "1 prewarm (image only)",
        vec![system.clone(), Message::user(vec![image_part.clone()])],
        1,
    )
    .await?;

    // 2. The real question: same image, now with text after it.
    let first_turn = vec![
        system.clone(),
        Message::user(vec![image_part.clone(), Part::Text(args.question.clone())]),
    ];
    let question = ask(&client, "2 question", first_turn.clone(), 96).await?;

    // 3. A follow-up on the same screenshot.
    let mut second_turn = first_turn;
    second_turn.push(Message::assistant(question.reply.clone()));
    second_turn.push(Message::user_text(args.follow_up.clone()));
    let follow_up = ask(&client, "3 follow-up", second_turn, 96).await?;

    println!();
    println!(
        "{:<24} {:>9} {:>9} {:>10} {:>9} {:>9} {:>8}",
        "request", "cached", "evaluated", "prompt ms", "ttft ms", "total ms", "tok/s"
    );
    for o in [&prewarm, &question, &follow_up] {
        println!(
            "{:<24} {:>9} {:>9} {:>10.0} {:>9} {:>9.0} {:>8.1}",
            o.label,
            o.cached(),
            o.evaluated(),
            o.timings.prompt_ms.unwrap_or(f64::NAN),
            o.ttft
                .map(|d| format!("{:.0}", ms(d)))
                .unwrap_or_else(|| "-".into()),
            ms(o.total),
            o.timings.predicted_per_second.unwrap_or(f64::NAN),
        );
    }

    if prewarm.timings.cache_n.is_none() && question.timings.cache_n.is_none() {
        bail!("the endpoint did not report `timings.cache_n`; this check needs llama-server");
    }

    // The prewarm and the question share system prompt + image. The few
    // tokens after the image differ (end-of-turn vs the question text), so
    // allow a small slack.
    const SLACK: u64 = 16;
    let image_prefix = prewarm.prompt_tokens();
    let reused = question.cached();
    println!();
    println!("image prefix: {image_prefix} tokens; reused by the question: {reused}");
    let question_ok = reused + SLACK >= image_prefix;
    let follow_up_ok = follow_up.cached() + SLACK >= question.prompt_tokens();
    println!(
        "question reuses the prewarmed image:   {}",
        if question_ok { "PASS" } else { "FAIL" }
    );
    println!(
        "follow-up reuses image + first turn:   {}",
        if follow_up_ok { "PASS" } else { "FAIL" }
    );
    if !(question_ok && follow_up_ok) {
        bail!("cached image prefix was not reused; the prewarm design needs rethinking");
    }
    Ok(())
}

async fn ask(
    client: &OpenAiClient,
    label: &'static str,
    messages: Vec<Message>,
    max_tokens: u32,
) -> Result<Outcome> {
    let mut request = ChatRequest {
        messages,
        max_tokens: Some(max_tokens),
        temperature: Some(0.0),
        ..Default::default()
    };
    request
        .extra_body
        .insert("cache_prompt".into(), json!(true));

    let started = Instant::now();
    let mut stream = client.chat(request).await?;
    let mut ttft = None;
    let mut reply = String::new();
    let mut summary = None;
    while let Some(event) = stream.next().await {
        match event? {
            ChatEvent::Content(text) => {
                ttft.get_or_insert_with(|| started.elapsed());
                reply.push_str(&text);
            }
            ChatEvent::Reasoning(_) => {}
            ChatEvent::Done(done) => summary = Some(done),
        }
    }
    let total = started.elapsed();
    let summary = summary.context("stream ended without a summary")?;
    Ok(Outcome {
        label,
        ttft,
        total,
        timings: summary.timings.unwrap_or_default(),
        reply,
    })
}
