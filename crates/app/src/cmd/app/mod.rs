//! `glance app`: the menu-bar app. Hold a chord, ask about the screen, hear
//! the answer.
//!
//! Threads (`docs/DESIGN.md` §2):
//! - The main thread runs `NSApplication` with the Accessory policy (no Dock
//!   icon). The same run loop delivers hotkey events, tray clicks, and the
//!   system voice's callbacks.
//! - The tokio runtime runs the orchestrator. Tray changes go back to the
//!   main thread through the main dispatch queue, because AppKit objects may
//!   only be touched there.

mod hotkeys;
mod tray;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use dispatch2::DispatchQueue;
use glance_audio::Microphone;
use glance_capture::live::{LiveScreen, request_screen_recording, screen_recording_granted};
use glance_llm_client::{Endpoint, OpenAiClient};
use glance_orchestrator::{
    Orchestrator, Outcome, Parts, TurnConfig, TurnReport, TurnState, Update,
};
use glance_stt::Parakeet;
use glance_tts::system_voice::SystemVoice;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use super::say::VoiceArgs;
use super::{find_model, manifest, store};
use tray::View;

#[derive(clap::Args)]
pub struct Args {
    /// OpenAI-compatible API root, e.g. http://127.0.0.1:8080/v1 (loopback only).
    #[arg(long)]
    endpoint: String,
    /// Model name to request from the endpoint.
    #[arg(long, default_value = "default")]
    model: String,
    /// Speech model id from models.toml.
    #[arg(long, default_value = "parakeet-tdt-0.6b-v3-int8")]
    stt: String,
    /// Longest edge of the image sent to the model, in pixels.
    #[arg(long, default_value_t = 1024)]
    max_edge: u32,
    /// Largest capture taken from the display, in pixels.
    #[arg(long, default_value_t = 2048)]
    capture_edge: u32,
    #[arg(long, default_value_t = 200)]
    max_tokens: u32,
    /// Do not send the image ahead of the question.
    #[arg(long)]
    no_prewarm: bool,
    #[command(flatten)]
    voice: VoiceArgs,
}

pub fn run(args: Args) -> Result<()> {
    let mtm = MainThreadMarker::new().context("glance app must run on the main thread")?;
    // Checked before anything starts, so a bad endpoint fails at once.
    let endpoint = Endpoint::loopback(&args.endpoint, &args.model)?;
    let host = endpoint
        .chat_url()
        .host_str()
        .map(|h| {
            format!(
                "{h}:{}",
                endpoint.chat_url().port_or_known_default().unwrap_or(0)
            )
        })
        .unwrap_or_default();
    let client = OpenAiClient::new(endpoint)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // Hotkeys first: if a chord cannot be registered, say which and do not
    // run the pipeline, so glance never sits there without a working chord.
    let (keys, key_events) = unbounded_channel();
    let registered = hotkeys::register(keys);
    let model_line = format!("Model: {} at {host}", args.model);

    // The tray may only be created once the run loop is running; the main
    // queue runs this block as soon as it is.
    let tray_model_line = model_line.clone();
    DispatchQueue::main().exec_async(move || tray::install(&tray_model_line));

    let _hotkeys = match registered {
        Ok(hotkeys) => {
            eprintln!(
                "glance: chords registered: {} (talk), {} (look)",
                hotkeys::TALK_LABEL,
                hotkeys::LOOK_LABEL
            );
            runtime.spawn(start(args, client, key_events));
            Some(hotkeys)
        }
        Err(message) => {
            eprintln!("glance: error: {message}");
            show(View::error(format!("Hotkey error: {message}")));
            None
        }
    };

    // Screen Recording: ask at launch rather than at the first question.
    // macOS shows its prompt once; a grant needs a relaunch to take effect.
    if !screen_recording_granted() {
        eprintln!("glance: Screen Recording is not granted; asking (relaunch after granting)");
        request_screen_recording();
    }
    runtime.spawn(refresh_permissions());

    eprintln!("glance: running ({model_line}). Quit from the menu-bar icon.");
    app.run();
    Ok(())
}

/// Load the speech model, build the pipeline, then run turns until exit.
async fn start(
    args: Args,
    client: OpenAiClient,
    key_events: UnboundedReceiver<glance_orchestrator::HotkeyEvent>,
) {
    show(View::status("Loading the speech model…"));
    let stt_id = args.stt.clone();
    let loaded = tokio::task::spawn_blocking(move || -> Result<Parakeet> {
        let manifest = manifest()?;
        let model = find_model(&manifest, &stt_id)?;
        let dir = store()?
            .verified_dir(model)
            .with_context(|| format!("run `glance models pull {}` first", model.id))?;
        Ok(Parakeet::load(&dir)?)
    })
    .await;
    let stt = match loaded {
        Ok(Ok(stt)) => stt,
        Ok(Err(e)) => return fail(format!("speech model: {e:#}")),
        Err(e) => return fail(format!("speech model: {e}")),
    };

    let (voice, speech_events) = SystemVoice::new(args.voice.config());
    let parts = Parts {
        capture: Arc::new(LiveScreen::new(args.capture_edge.max(args.max_edge))),
        mic: Box::new(Microphone::new(60)),
        stt: Box::new(stt),
        chat: Arc::new(client),
        tts: Arc::new(voice),
        speech_events,
    };
    let config = TurnConfig {
        max_edge: args.max_edge,
        max_tokens: args.max_tokens,
        prewarm: !args.no_prewarm,
        ..Default::default()
    };
    let (orchestrator, updates) = Orchestrator::new(parts, config);
    tokio::spawn(forward_updates(updates));
    show(View::state(TurnState::Idle));
    eprintln!("glance: ready");
    orchestrator.run(key_events).await;
}

fn fail(message: String) {
    eprintln!("glance: error: {message}");
    show(View::error(message));
}

/// Turn orchestrator updates into tray changes and log lines.
async fn forward_updates(mut updates: UnboundedReceiver<Update>) {
    while let Some(update) = updates.recv().await {
        match update {
            Update::State(state) => show(View::state(state)),
            Update::Report(report) => {
                log_report(&report);
                if let Outcome::Failed(error) = &report.outcome {
                    show(View::turn_failed(error.to_string()));
                }
            }
        }
    }
}

/// One line per turn on stderr: timings and counts only (`docs/DESIGN.md`
/// §11, "Logs").
fn log_report(report: &TurnReport) {
    eprintln!("glance: {report}");
    if let Outcome::Failed(error) = &report.outcome {
        eprintln!("glance: turn {} failed: {error}", report.turn);
    }
}

/// Permissions can change in System Settings at any time; re-read them
/// every few seconds so the checklist stays true.
async fn refresh_permissions() {
    let mut tick = tokio::time::interval(Duration::from_secs(3));
    loop {
        tick.tick().await;
        DispatchQueue::main().exec_async(tray::refresh_permissions);
    }
}

/// Apply a tray change on the main thread.
fn show(view: View) {
    DispatchQueue::main().exec_async(move || tray::apply(view));
}
