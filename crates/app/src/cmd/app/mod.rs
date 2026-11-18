//! `glance app`: the menu-bar app. Hold a chord, ask about the screen, hear
//! the answer.
//!
//! This file is the portable part: models, speech and the orchestrator,
//! talking to the user through the `Shell` trait. The platform part (the
//! main-thread run loop, hotkeys, menu-bar icon) is `glance_macos::shell`.
//! The tokio runtime runs the orchestrator beside the main thread.

mod settings;
mod tiers;

use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use glance_audio::Microphone;
use glance_llm_client::{Endpoint, OpenAiClient};
use glance_macos::capture::LiveScreen;
use glance_macos::hotkeys::Chords;
use glance_orchestrator::{
    Controller, DeepStatus, DeepTier, Orchestrator, Outcome, Parts, Resident, Shell, TurnConfig,
    TurnReport, TurnState, Update,
};
use glance_stt::Parakeet;
use tokio::sync::mpsc::UnboundedReceiver;

use super::speech::{self, SpeechArgs};
use super::{find_model, manifest, store};
use crate::config::{Config, ModelsConfig, SpeechConfig};
use tiers::Source;

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
    /// Play a short scripted turn on the overlay at start-up (fixed sample
    /// text, nothing is captured or asked), to check how it looks.
    #[arg(long)]
    overlay_demo: bool,
    /// Open the settings window at start-up (also in the menu-bar menu).
    #[arg(long)]
    open_settings: bool,
    #[command(flatten)]
    speech: SpeechArgs,
}

pub fn run(args: Args) -> Result<()> {
    // Read the config file now, so a mistake in it (or a bad endpoint)
    // stops the launch with a message instead of surfacing later.
    let settings = args.speech.settings()?;
    let config_file = Config::load()?;
    let models = config_file.models.clone();
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
    // DEEP, if configured, is loaded on demand ("think harder"), or at
    // start-up with auto_escalate.
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
    let model_line = format!(
        "FAST: {} · DEEP: {}",
        fast.label(),
        deep.as_ref().map_or("none".to_owned(), Source::label)
    );
    let chords = Chords {
        talk: config_file.hotkeys.talk.clone(),
        look: config_file.hotkeys.look.clone(),
        think: config_file.hotkeys.think.clone(),
    };
    let options = glance_macos::shell::Options {
        chords,
        model_line,
        on_quit: Box::new(tiers::shutdown),
        on_new_session: Box::new(new_session),
        overlay: config_file.overlay.enabled,
        settings: Some(
            settings::backend() as Arc<dyn glance_orchestrator::settings::SettingsBackend>
        ),
    };
    let handle = runtime.handle().clone();
    glance_macos::shell::run(options, move |key_events, shell| {
        // No key events: a chord did not register; the shell shows why.
        if let Some(key_events) = key_events {
            if args.open_settings {
                glance_macos::shell::open_settings();
            }
            if args.overlay_demo {
                handle.spawn(overlay_demo(Arc::clone(&shell)));
            }
            handle.spawn(start(args, models, settings, fast, deep, key_events, shell));
        }
    })
    .map_err(anyhow::Error::msg)
}

/// Load the speech model, build the pipeline, then run turns until exit.
async fn start(
    args: Args,
    models: ModelsConfig,
    settings: SpeechConfig,
    fast: Source,
    deep: Option<Source>,
    key_events: UnboundedReceiver<glance_orchestrator::HotkeyEvent>,
    shell: Arc<dyn Shell>,
) {
    shell.status("Starting the models…");
    // A crashed glance may have left servers running; stop them first.
    tokio::task::spawn_blocking(super::clean_stale_servers)
        .await
        .ok();
    let fail = |message: String| {
        eprintln!("glance: error: {message}");
        shell.fatal(&message);
    };
    // FAST loads while speech loads; DEEP waits until it is asked for.
    let fast = tokio::spawn(tiers::start(fast));
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
    let (voice, speech_events, speech_control) = (started.tts, started.events, started.control);
    let fast = match fast.await {
        Ok(Ok(tier)) => tier,
        Ok(Err(e)) => return fail(format!("FAST model: {e:#}")),
        Err(e) => return fail(format!("FAST model: {e}")),
    };
    let deep: Option<Arc<dyn DeepTier>> = match deep {
        None => None,
        Some(Source::External { endpoint, model }) => {
            match Endpoint::loopback(&endpoint, model)
                .map_err(anyhow::Error::from)
                .and_then(|e| OpenAiClient::new(e).map_err(anyhow::Error::from))
            {
                Ok(client) => Some(Arc::new(Resident(Arc::new(client)))),
                Err(e) => {
                    eprintln!("glance: DEEP endpoint unusable, continuing without it: {e:#}");
                    None
                }
            }
        }
        Some(Source::Managed { id, ctx, args }) => {
            let deep = tiers::OnDemandDeep::new(id, ctx, args);
            if models.auto_escalate {
                // FAST may escalate at any time: load DEEP now.
                let loading = Arc::clone(&deep);
                shell.update(&Update::Deep(DeepStatus::Loading));
                let status_shell = Arc::clone(&shell);
                tokio::spawn(async move {
                    let status = match loading.load().await {
                        Ok(_) => DeepStatus::Loaded,
                        Err(e) => {
                            eprintln!("glance: DEEP did not start: {e}");
                            DeepStatus::NotLoaded
                        }
                    };
                    status_shell.update(&Update::Deep(status));
                });
            }
            Some(deep)
        }
    };
    if deep.is_some() {
        shell.update(&Update::Deep(DeepStatus::NotLoaded));
    }
    // Overlay text goes straight to the shell, never to a log.
    let (display_tx, mut display_rx) = tokio::sync::mpsc::unbounded_channel();
    let display_shell = Arc::clone(&shell);
    tokio::spawn(async move {
        while let Some(shown) = display_rx.recv().await {
            display_shell.display(&shown);
        }
    });
    let deep_max_edge = TurnConfig::default().deep_max_edge;
    let mut deep_body = TurnConfig::default().deep_extra_body;
    deep_body.insert("chat_template_kwargs".into(), models.deep_template_kwargs());
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
        deep,
        tts: voice,
        speech_events,
        display: Some(display_tx),
    };
    let config = TurnConfig {
        max_edge: args.max_edge,
        max_tokens: args.max_tokens,
        prewarm: !args.no_prewarm,
        grammar: fast.grammar,
        deep_extra_body: deep_body,
        auto_escalate: models.auto_escalate,
        response_length: models.response_length(),
        deep_idle: std::time::Duration::from_secs(models.deep_idle_secs),
        ..Default::default()
    };
    let (orchestrator, updates) = Orchestrator::new(parts, config);
    let _ = CONTROLLER.set(orchestrator.controller());
    settings::backend().attach(orchestrator.controller(), speech_control);
    tokio::spawn(forward_updates(updates, Arc::clone(&shell)));
    shell.update(&Update::State(TurnState::Idle));
    eprintln!("glance: ready");
    orchestrator.run(key_events).await;
}

/// Set once the pipeline is running; the tray's "New session" uses it.
static CONTROLLER: OnceLock<Controller> = OnceLock::new();

/// End the session (tray menu). Does nothing before the pipeline is up.
fn new_session() {
    if let Some(controller) = CONTROLLER.get() {
        controller.new_session();
    }
}

/// `--overlay-demo`: a scripted turn with fixed text, through the real shell.
async fn overlay_demo(shell: Arc<dyn Shell>) {
    use glance_orchestrator::Shown;
    use std::time::Duration;
    let pause = |ms| tokio::time::sleep(Duration::from_millis(ms));
    pause(1500).await;
    shell.display(&Shown::Clear);
    shell.update(&Update::State(TurnState::Listening));
    pause(1500).await;
    shell.display(&Shown::Asked("What does this chart show?".into()));
    shell.update(&Update::State(TurnState::Thinking));
    pause(700).await;
    shell.update(&Update::State(TurnState::Speaking));
    for sentence in [
        "Weekly active users rose from 1,240 to 1,720 over four weeks.",
        "Week three dipped slightly before the biggest jump in week four.",
    ] {
        shell.display(&Shown::Said(sentence.into()));
        pause(1800).await;
    }
    shell.update(&Update::State(TurnState::Idle));
}

/// Orchestrator updates to the shell, plus the per-turn log line.
async fn forward_updates(mut updates: UnboundedReceiver<Update>, shell: Arc<dyn Shell>) {
    while let Some(update) = updates.recv().await {
        match &update {
            Update::Report(report) => log_report(report),
            Update::Session(info) if info.questions == 0 => eprintln!("glance: session ended"),
            _ => {}
        }
        shell.update(&update);
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
