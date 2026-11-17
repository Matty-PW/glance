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
mod tiers;
mod tray;

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use dispatch2::DispatchQueue;
use glance_audio::Microphone;
use glance_capture::live::{LiveScreen, request_screen_recording, screen_recording_granted};
use glance_orchestrator::{
    Controller, Orchestrator, Outcome, Parts, TurnConfig, TurnReport, TurnState, Update,
};
use glance_stt::Parakeet;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use super::speech::{self, SpeechArgs};
use super::{find_model, manifest, store};
use crate::config::{Config, SpeechConfig};
use tiers::Source;
use tray::View;

#[derive(clap::Args)]
pub struct Args {
    /// Use your own FAST server instead of the one glance runs: an
    /// OpenAI-compatible API root such as http://127.0.0.1:8080/v1
    /// (loopback only). Overrides `[models]` in the config file.
    #[arg(long)]
    endpoint: Option<String>,
    /// Model name to request from --endpoint.
    #[arg(long, default_value = "default")]
    model: String,
    /// Run without a DEEP tier even if the config file names one.
    #[arg(long)]
    no_deep: bool,
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
    speech: SpeechArgs,
}

pub fn run(args: Args) -> Result<()> {
    let mtm = MainThreadMarker::new().context("glance app must run on the main thread")?;
    // Read the config file now, so a mistake in it (or a bad endpoint)
    // stops the launch with a message instead of surfacing later.
    let settings = args.speech.settings()?;
    let models = Config::load()?.models;
    let fast = match &args.endpoint {
        Some(endpoint) => Source::External {
            endpoint: endpoint.clone(),
            model: args.model.clone(),
        },
        None => match &models.fast_endpoint {
            Some(endpoint) => Source::External {
                endpoint: endpoint.clone(),
                model: models.fast_model.clone(),
            },
            None => Source::Managed {
                id: models.fast.clone(),
                ctx: models.fast_ctx,
                args: Vec::new(),
            },
        },
    };
    let deep = match (&models.deep_endpoint, &models.deep) {
        _ if args.no_deep => None,
        (Some(endpoint), _) => Some(Source::External {
            endpoint: endpoint.clone(),
            model: models.deep_model.clone(),
        }),
        (None, Some(id)) => Some(Source::Managed {
            id: id.clone(),
            ctx: models.deep_ctx,
            args: models.deep_server_args(),
        }),
        (None, None) => None,
    };
    fast.check().context("FAST model")?;
    if let Some(deep) = &deep {
        deep.check().context("DEEP model")?;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // Hotkeys first: if a chord cannot be registered, say which and do not
    // run the pipeline, so glance never sits there without a working chord.
    let (keys, key_events) = unbounded_channel();
    let registered = hotkeys::register(keys);
    let model_line = format!(
        "FAST: {} · DEEP: {}",
        fast.label(),
        deep.as_ref().map_or("none".to_owned(), Source::label)
    );

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
            let deep_kwargs = models.deep_template_kwargs();
            runtime.spawn(start(args, deep_kwargs, settings, fast, deep, key_events));
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
    runtime.spawn(refresh_tray());

    eprintln!("glance: running ({model_line}). Quit from the menu-bar icon.");
    app.run();
    Ok(())
}

/// Load the speech model, build the pipeline, then run turns until exit.
async fn start(
    args: Args,
    args_deep_kwargs: serde_json::Value,
    settings: SpeechConfig,
    fast: Source,
    deep: Option<Source>,
    key_events: UnboundedReceiver<glance_orchestrator::HotkeyEvent>,
) {
    show(View::status("Starting the models…"));
    // The language models load while speech loads.
    let fast = tokio::spawn(tiers::start(fast));
    let deep = deep.map(|d| tokio::spawn(tiers::start(d)));
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

    let settings = settings.clone();
    let started = tokio::task::spawn_blocking(move || speech::start(&settings)).await;
    let started = match started {
        Ok(started) => started,
        Err(e) => return fail(format!("speech engine: {e}")),
    };
    eprintln!("glance: speech: {}", started.description);
    let (voice, speech_events) = (started.tts, started.events);
    let fast = match fast.await {
        Ok(Ok(tier)) => tier,
        Ok(Err(e)) => return fail(format!("FAST model: {e:#}")),
        Err(e) => return fail(format!("FAST model: {e}")),
    };
    let deep = match deep {
        None => None,
        Some(task) => match task.await {
            Ok(Ok(tier)) => Some(tier),
            // FAST alone still works: its prompt then omits escalation.
            Ok(Err(e)) => {
                eprintln!("glance: DEEP model unavailable, continuing without it: {e:#}");
                None
            }
            Err(e) => {
                eprintln!("glance: DEEP model unavailable, continuing without it: {e}");
                None
            }
        },
    };
    let deep_max_edge = TurnConfig::default().deep_max_edge;
    let mut deep_body = TurnConfig::default().deep_extra_body;
    deep_body.insert("chat_template_kwargs".into(), args_deep_kwargs.clone());
    let capture_edge = if deep.is_some() {
        args.capture_edge.max(deep_max_edge)
    } else {
        args.capture_edge
    };
    let parts = Parts {
        capture: Arc::new(LiveScreen::new(capture_edge.max(args.max_edge))),
        mic: Box::new(Microphone::new(60)),
        stt: Box::new(stt),
        chat: Arc::new(fast.client),
        deep: deep
            .as_ref()
            .map(|d| -> Arc<dyn glance_llm_client::ChatModel> { Arc::new(d.client.clone()) }),
        tts: voice,
        speech_events,
    };
    let config = TurnConfig {
        max_edge: args.max_edge,
        max_tokens: args.max_tokens,
        prewarm: !args.no_prewarm,
        grammar: fast.grammar,
        deep_extra_body: deep_body,
        ..Default::default()
    };
    let (orchestrator, updates) = Orchestrator::new(parts, config);
    let _ = CONTROLLER.set(orchestrator.controller());
    tokio::spawn(forward_updates(updates));
    show(View::state(TurnState::Idle));
    eprintln!("glance: ready");
    orchestrator.run(key_events).await;
}

/// Quit: stop the servers glance started, then the app.
fn quit() {
    tiers::shutdown();
    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm).terminate(None);
    }
}

/// Set once the pipeline is running; the tray's "New session" uses it.
static CONTROLLER: OnceLock<Controller> = OnceLock::new();

/// End the session (tray menu). Does nothing before the pipeline is up.
fn new_session() {
    if let Some(controller) = CONTROLLER.get() {
        controller.new_session();
    }
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
            Update::Session(info) => {
                if info.questions == 0 {
                    eprintln!("glance: session ended");
                }
                show(View::session(info));
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

/// Permissions can change in System Settings at any time, and the
/// screenshot's age moves on: refresh both every few seconds.
async fn refresh_tray() {
    let mut tick = tokio::time::interval(Duration::from_secs(3));
    loop {
        tick.tick().await;
        DispatchQueue::main().exec_async(tray::refresh);
    }
}

/// Apply a tray change on the main thread.
fn show(view: View) {
    DispatchQueue::main().exec_async(move || tray::apply(view));
}
