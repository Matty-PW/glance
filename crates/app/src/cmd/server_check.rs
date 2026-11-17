//! `glance server-check <model>`: start a managed llama-server the way the
//! app will (loopback, random port, per-launch key, vision check), then
//! check that the escalation grammar works with streaming: one escalating
//! and one answering request, text only. Prints timings and the decision,
//! never model output beyond those two short replies.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use glance_llm_client::escalate::{Decision, EscalationGate, GRAMMAR, Step};
use glance_llm_client::server::{ManagedServer, ServerSpec};
use glance_llm_client::{ChatEvent, ChatModel, ChatRequest, Message, OpenAiClient};
use glance_models::{Kind, Role};
use serde_json::json;

use super::{find_model, manifest, ms, store};

#[derive(clap::Args)]
pub struct Args {
    /// Vision model id from models.toml.
    id: String,
    #[arg(long, default_value_t = 8192)]
    ctx: u32,
    /// Show llama-server's own output.
    #[arg(long)]
    show_output: bool,
}

pub async fn run(args: Args) -> Result<()> {
    let manifest = manifest()?;
    let model = find_model(&manifest, &args.id)?;
    if model.role != Role::Vlm {
        bail!("`{}` is not a vision-language model", model.id);
    }
    let store = store()?;
    let dir = store
        .verified_dir(model)
        .with_context(|| format!("run `glance models pull {}` first", model.id))?;
    let file = |kind: Kind| {
        model
            .files
            .iter()
            .find(|f| f.kind == kind)
            .map(|f| dir.join(&f.path))
            .with_context(|| format!("{} has no {kind:?} file", model.id))
    };
    let spec = ServerSpec {
        weights: file(Kind::Weights)?,
        mmproj: file(Kind::Mmproj)?,
        alias: model.id.clone(),
        ctx: args.ctx,
        show_output: args.show_output,
        start_timeout: Duration::from_secs(180),
        extra_args: Vec::new(),
    };
    let started = Instant::now();
    let server = ManagedServer::start(&spec).await?;
    println!(
        "started {server:?} in {:.1} s; /props reports vision",
        started.elapsed().as_secs_f64()
    );
    let client = OpenAiClient::new(server.endpoint().clone())?;

    let system = "If the user says ESCALATE, output exactly \
        {\"glance\":\"escalate\",\"v\":1,\"reason\":\"user_asked\"} and nothing else. \
        Otherwise answer in one short sentence.";
    for (question, expect) in [
        ("ESCALATE", "escalate"),
        ("What colour is the sky on a clear day?", "answer"),
    ] {
        let mut request = ChatRequest {
            messages: vec![Message::system(system), Message::user_text(question)],
            max_tokens: Some(40),
            temperature: Some(0.0),
            ..Default::default()
        };
        request.extra_body.insert("grammar".into(), json!(GRAMMAR));
        request.extra_body.insert(
            "chat_template_kwargs".into(),
            json!({"enable_thinking": false}),
        );
        let t = Instant::now();
        let mut stream = client.chat(request).await?;
        let mut gate = EscalationGate::new();
        let mut first_token = None;
        let mut spoken = String::new();
        let mut decision = None;
        while let Some(event) = stream.next().await {
            match event? {
                ChatEvent::Content(text) => {
                    first_token.get_or_insert_with(|| t.elapsed());
                    match gate.push(&text) {
                        Step::Wait => {}
                        Step::Speak(s) => spoken.push_str(&s),
                        Step::Stop(d) => {
                            decision = Some(d);
                            break;
                        }
                    }
                }
                ChatEvent::Reasoning(_) => {}
                ChatEvent::Done(_) => break,
            }
        }
        let decision = decision.unwrap_or_else(|| gate.finish());
        let ok = matches!(
            (expect, decision),
            ("escalate", Decision::Escalate(_)) | ("answer", Decision::Answer)
        );
        println!(
            "{:<8} expected {expect:<8} got {decision:?} (first token {:.0} ms){}",
            if ok { "ok" } else { "MISMATCH" },
            first_token.map_or(0.0, ms),
            if spoken.is_empty() {
                String::new()
            } else {
                format!(": \"{}\"", spoken.trim())
            }
        );
    }
    drop(server);
    println!("server stopped");
    Ok(())
}
