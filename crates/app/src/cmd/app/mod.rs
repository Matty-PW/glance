//! `glance app`: the menu-bar app. Hold a chord, ask about the screen, hear
//! the answer.
//!
//! This file is the portable part: models, speech and the orchestrator,
//! talking to the user through the `Shell` trait. The platform part (the
//! main-thread run loop, hotkeys, menu-bar icon) is `glance_macos::shell`.
//! The tokio runtime runs the orchestrator beside the main thread.

mod models;
mod settings;
pub mod setup;
pub mod setup_demo;
mod tiers;

use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use glance_audio::{LevelMeter, Microphone};
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
    /// Open the models window at start-up (also in the menu-bar menu).
    #[arg(long)]
    open_models: bool,
    /// Open the setup window at start-up, from its first page (also
    /// "Set up glance…" in the menu-bar menu).
    #[arg(long)]
    open_setup: bool,
    /// Walk through the setup window on made-up data (a fake download with
    /// a failure, a restart-needed permission, a failing and a working
    /// test). Touches no real permission, download, model or config; the
    /// pipeline does not start.
    #[arg(long)]
    setup_demo: bool,
    #[command(flatten)]
    speech: SpeechArgs,
}

/// Where FAST and DEEP run, from the arguments and `[models]`.
fn sources(args: &Args, models: &ModelsConfig) -> (Source, Option<Source>) {
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
    (fast, deep)
}

/// Whether the setup window opens at launch, and where.
fn setup_at(args: &Args, setup: &setup::AppSetup) -> Option<glance_orchestrator::setup::Step> {
    use glance_macos::permission::{mic_status, screen_recording_fresh, screen_recording_granted};
    use glance_orchestrator::setup::{
        LaunchFacts, Step, classify_mic, classify_screen, launch_decision,
    };
    if args.open_setup {
        return Some(Step::Welcome);
    }
    let state = setup.state();
    let granted = screen_recording_granted();
    // The child-process check only matters when this process says no.
    let fresh = if granted {
        None
    } else {
        screen_recording_fresh()
    };
    launch_decision(LaunchFacts {
        completed: state.completed,
        screen: classify_screen(granted, fresh, state.screen_asked),
        mic: classify_mic(mic_status()),
        models_ready: setup.configured_ready(),
        saved_step: state.saved_step(),
    })
}

pub fn run(args: Args) -> Result<()> {
    // Read the config file now, so a mistake in it (or a bad endpoint)
    // stops the launch with a message instead of surfacing later.
    let settings = args.speech.settings()?;
    let config_file = Config::load()?;
    let models = config_file.models.clone();
    let (fast, deep) = sources(&args, &models);
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
    let models_panel = models::ModelsManager::new(
        manifest()?,
        store()?,
        runtime.handle().clone(),
        models::in_use(args.stt.clone(), config_file.clone()),
    );
    let models_backend = Arc::new(models::Models(models_panel));
    let quitting = Arc::clone(&models_backend);
    let setup_backend = setup::AppSetup::new(
        Arc::clone(&models_backend) as Arc<dyn glance_orchestrator::settings::ModelsBackend>,
        args.stt.clone(),
        matches!(fast, Source::External { .. }),
    );
    let _ = SETUP.set(Arc::clone(&setup_backend));
    let demo = args
        .setup_demo
        .then(|| setup_demo::DemoSetup::new(runtime.handle().clone()));
    let open_setup_at = if demo.is_some() {
        eprintln!(
            "glance: setup demo: made-up data only; nothing real is asked, downloaded or saved"
        );
        Some(glance_orchestrator::setup::Step::Welcome)
    } else {
        setup_at(&args, &setup_backend)
    };
    if let Some(step) = open_setup_at {
        eprintln!("glance: setup: opening at step={}", step.as_str());
    }
    let options = glance_macos::shell::Options {
        chords,
        model_line,
        on_quit: Box::new(move || {
            // Stop a running download and keep what arrived: the next
            // download of that model resumes it.
            glance_orchestrator::settings::ModelsBackend::pause(&*quitting);
            tiers::shutdown();
        }),
        on_new_session: Box::new(new_session),
        overlay: config_file.overlay.enabled,
        settings: Some(settings::backend()),
        models: Some(models_backend),
        setup: Some(match &demo {
            Some(demo) => Arc::clone(demo) as Arc<dyn glance_orchestrator::setup::SetupBackend>,
            None => Arc::clone(&setup_backend) as Arc<dyn glance_orchestrator::setup::SetupBackend>,
        }),
        setup_at: open_setup_at,
        setup_demo: demo.is_some(),
        mic_level: Some(Box::new(|| {
            if DEMO_RUNNING.load(std::sync::atomic::Ordering::Relaxed) {
                demo_level()
            } else {
                MIC_LEVEL.get_or_init(LevelMeter::default).get()
            }
        })),
    };
    let handle = runtime.handle().clone();
    glance_macos::shell::run(options, move |key_events, shell| {
        // No key events: a chord did not register; the shell shows why.
        if let Some(key_events) = key_events {
            if args.open_settings {
                glance_macos::shell::open_settings();
            }
            if args.open_models {
                glance_macos::shell::open_models();
            }
            if args.overlay_demo {
                handle.spawn(overlay_demo(Arc::clone(&shell)));
            }
            let _ = (fast, deep, models);
            if let Some(demo) = &demo {
                // The demo plays the overlay itself; no pipeline starts.
                demo.attach(Arc::clone(&shell));
                shell.status("Setup demo (no models are loaded)");
                return;
            }
            handle.spawn(start_when_ready(
                args,
                settings,
                key_events,
                shell,
                setup_backend,
            ));
        }
    })
    .map_err(anyhow::Error::msg)
}

/// Wait until the configured models are on disk (setup or the Models page
/// downloads them), then start the pipeline with the config as it is then:
/// setup may have chosen other models meanwhile.
async fn start_when_ready(
    args: Args,
    settings: SpeechConfig,
    key_events: UnboundedReceiver<glance_orchestrator::HotkeyEvent>,
    shell: Arc<dyn Shell>,
    setup: Arc<setup::AppSetup>,
) {
    let ready = setup.ready();
    let mut said = false;
    loop {
        let notified = ready.notified();
        if setup.configured_ready() {
            break;
        }
        if !said {
            said = true;
            eprintln!("glance: waiting for setup: the models are not downloaded yet");
            shell.status("Waiting for setup: the models are not downloaded yet");
        }
        // Setup notifies; the Models page does not, so look again now and
        // then as well.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), notified).await;
    }
    let config = match Config::load() {
        Ok(config) => config,
        Err(e) => {
            let message = format!("config.toml: {e:#}");
            eprintln!("glance: error: {message}");
            setup.set_broken(message.clone());
            return shell.fatal(&message);
        }
    };
    let models = config.models.clone();
    let (fast, deep) = sources(&args, &models);
    if let Source::Managed { id, .. } = &fast {
        let deep_id = match &deep {
            Some(Source::Managed { id, .. }) => Some(id.clone()),
            _ => None,
        };
        setup.set_running(id.clone(), deep_id);
    }
    start(args, models, settings, fast, deep, key_events, shell, setup).await;
}

/// Load the speech model, build the pipeline, then run turns until exit.
#[allow(clippy::too_many_arguments)]
async fn start(
    args: Args,
    models: ModelsConfig,
    settings: SpeechConfig,
    fast: Source,
    deep: Option<Source>,
    mut key_events: UnboundedReceiver<glance_orchestrator::HotkeyEvent>,
    shell: Arc<dyn Shell>,
    setup: Arc<setup::AppSetup>,
) {
    shell.status("Starting the models…");
    // A crashed glance may have left servers running; stop them first.
    tokio::task::spawn_blocking(super::clean_stale_servers)
        .await
        .ok();
    let fail = |message: String| {
        eprintln!("glance: error: {message}");
        setup.set_broken(message.clone());
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

    let math_as_words = settings.math_as_words;
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
    let capture = Arc::new(TestableCapture {
        live: LiveScreen::new(capture_edge.max(args.max_edge)),
        sample: std::sync::Mutex::new(None),
    });
    let parts = Parts {
        capture: Arc::clone(&capture) as Arc<dyn glance_capture::ScreenCapture>,
        mic: Box::new(
            Microphone::new(60)
                .with_level_meter(MIC_LEVEL.get_or_init(LevelMeter::default).clone()),
        ),
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
        personality: models.personality(),
        math_as_words,
        deep_idle: std::time::Duration::from_secs(models.deep_idle_secs),
        ..Default::default()
    };
    let (orchestrator, updates) = Orchestrator::new(parts, config);
    let _ = CONTROLLER.set(orchestrator.controller());
    settings::backend().attach(orchestrator.controller(), speech_control);
    // The chords go through a relay so setup's test question can press and
    // release one itself; real presses are dropped while it runs.
    let (keys, relayed) = tokio::sync::mpsc::unbounded_channel();
    let relay = keys.clone();
    tokio::spawn(async move {
        while let Some(event) = key_events.recv().await {
            if !TEST_ACTIVE.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = relay.send(event);
            }
        }
    });
    setup.set_hooks(test_hooks(keys, capture, Arc::clone(&setup)));
    tokio::spawn(forward_updates(updates, Arc::clone(&shell)));
    shell.update(&Update::State(TurnState::Idle));
    eprintln!("glance: ready");
    orchestrator.run(relayed).await;
}

/// The setup backend, for the per-turn updates (the test question's result).
static SETUP: OnceLock<Arc<setup::AppSetup>> = OnceLock::new();
/// Setup's test question is running: its turn is the next report, and real
/// chord presses are held back.
static TEST_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The live screen, except for one turn after setup's test question asks
/// for the built-in sample chart instead.
struct TestableCapture {
    live: LiveScreen,
    sample: std::sync::Mutex<Option<glance_capture::Screenshot>>,
}

impl glance_capture::ScreenCapture for TestableCapture {
    fn capture(&self) -> Result<glance_capture::Screenshot, glance_capture::CaptureError> {
        let sample = self.sample.lock().unwrap_or_else(|p| p.into_inner()).take();
        match sample {
            Some(shot) => Ok(shot),
            None => self.live.capture(),
        }
    }
}

/// The test question: press the "look" chord for the user (so a fresh
/// image is taken, which is the sample chart), record until Stop or
/// `TEST_LISTEN`, release. The result comes back with the turn's report.
fn test_hooks(
    keys: tokio::sync::mpsc::UnboundedSender<glance_orchestrator::HotkeyEvent>,
    capture: Arc<TestableCapture>,
    setup: Arc<setup::AppSetup>,
) -> setup::TestHooks {
    use glance_orchestrator::setup::{TEST_LISTEN, TestResult};
    use glance_orchestrator::{Chord, HotkeyEvent};
    use std::sync::atomic::Ordering;
    let (stop_keys, stop_setup) = (keys.clone(), Arc::clone(&setup));
    let release = move |keys: &tokio::sync::mpsc::UnboundedSender<HotkeyEvent>,
                        setup: &setup::AppSetup| {
        if setup.test() == TestResult::Listening {
            setup.set_test(TestResult::Working);
            let _ = keys.send(HotkeyEvent::Released(Chord::Look));
        }
    };
    setup::TestHooks {
        start: Box::new(move || {
            // Drawn on the main thread (the window's button), in memory.
            let png = glance_macos::sample::chart_png()?;
            let shot = glance_capture::Screenshot::from_png(&png)
                .map_err(|e| format!("the test image: {e}"))?;
            *capture.sample.lock().unwrap_or_else(|p| p.into_inner()) = Some(shot);
            new_session();
            TEST_ACTIVE.store(true, Ordering::Relaxed);
            setup.set_test(TestResult::Listening);
            eprintln!("glance: setup: test started");
            keys.send(HotkeyEvent::Pressed(Chord::Look))
                .map_err(|_| "glance's pipeline has stopped; quit and start it again".to_owned())?;
            let (keys, setup) = (keys.clone(), Arc::clone(&setup));
            std::thread::spawn(move || {
                std::thread::sleep(TEST_LISTEN);
                release(&keys, &setup);
            });
            Ok(())
        }),
        stop: Box::new(move || release(&stop_keys, &stop_setup)),
    }
}

/// The test turn's report: a result for the Try it page, and a log line
/// with the stage code and timing only.
fn test_report(report: &TurnReport) {
    use glance_orchestrator::setup::{Stage, TestResult, stage_of};
    use std::sync::atomic::Ordering;
    if !TEST_ACTIVE.swap(false, Ordering::Relaxed) {
        return;
    }
    // Leave nothing of the test in the session: the next question uses the
    // real screen.
    new_session();
    let result = match &report.outcome {
        Outcome::Answered => TestResult::Works {
            first_word: report.ttfa.or(report.total).unwrap_or_default(),
        },
        Outcome::Failed(error) => {
            let (stage, hint) = stage_of(error);
            TestResult::Failed {
                stage,
                hint: hint.to_owned(),
            }
        }
        Outcome::ShortPress | Outcome::Cancelled => TestResult::Failed {
            stage: Stage::SpeechToText,
            hint: "The recording was cut short. Try again, and press Stop only after asking."
                .to_owned(),
        },
    };
    match &result {
        TestResult::Works { first_word } => eprintln!(
            "glance: setup: test stage=ok ttfa_ms={}",
            first_word.as_millis()
        ),
        TestResult::Failed { stage, .. } => {
            eprintln!("glance: setup: test stage={} failed", stage.code())
        }
        _ => {}
    }
    if let Some(setup) = SETUP.get() {
        setup.set_test(result);
    }
}

/// The microphone's level, shared by the microphone and the overlay.
static MIC_LEVEL: OnceLock<LevelMeter> = OnceLock::new();
/// `--overlay-demo` is playing: the overlay shows a made-up level.
static DEMO_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A speech-like level for the demo: syllables of varying loudness.
fn demo_level() -> f32 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f32() % 1000.0);
    let syllables = (t * 7.0).sin().abs();
    let phrase = 0.5 + 0.5 * (t * 1.3).sin();
    0.02 + 0.12 * syllables * phrase
}

/// Set once the pipeline is running; the tray's "New session" uses it.
static CONTROLLER: OnceLock<Controller> = OnceLock::new();

/// End the session (tray menu). Does nothing before the pipeline is up.
fn new_session() {
    if let Some(controller) = CONTROLLER.get() {
        controller.new_session();
    }
}

/// `--overlay-demo`: scripted turns with fixed text, through the real shell,
/// cycling Listening (with a made-up mic level), Thinking and Speaking,
/// twice: a short answer on a new screenshot (the indicator plays on the
/// main display, with the current setting), then a "think harder" one with
/// a longer answer on the same screenshot (no indicator; the overlay says
/// how old it is). Then a tour of the indicator's looks, whatever the
/// setting, each named in the overlay.
async fn overlay_demo(shell: Arc<dyn Shell>) {
    use glance_orchestrator::{Progress, ScreenshotUse, Shown};
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};
    let pause = |ms| tokio::time::sleep(Duration::from_millis(ms));
    let rounds: [(&str, Option<Progress>, &[&str]); 2] = [
        (
            "What does this chart show?",
            None,
            &[
                "Weekly active users rose from 1,240 to 1,720 over four weeks.",
                "Week three dipped slightly before the biggest jump in week four.",
            ],
        ),
        (
            "Think harder: why did week three dip?",
            Some(Progress::ThinkingHarder),
            &[
                "Week three is the only week where sign-ups fell, from 410 to 365.",
                "The note under the chart says the onboarding email was paused that week.",
                "Returning users held steady, so the dip comes from fewer new users, not lost ones.",
                "When the email came back in week four, sign-ups jumped to 520.",
            ],
        ),
    ];
    pause(1500).await;
    let mut taken = None;
    for (question, progress, sentences) in rounds {
        shell.display(&Shown::Clear);
        DEMO_RUNNING.store(true, Ordering::Relaxed);
        shell.update(&Update::State(TurnState::Listening));
        // The first round takes a screenshot (a capture takes ~50 ms); the
        // think-harder round reuses it.
        pause(60).await;
        let used = match taken {
            None => {
                taken = Some(Instant::now());
                ScreenshotUse::Fresh { display: None }
            }
            Some(taken) => ScreenshotUse::Reused { taken },
        };
        shell.update(&Update::Screenshot(used));
        pause(2440).await;
        DEMO_RUNNING.store(false, Ordering::Relaxed);
        shell.update(&Update::State(TurnState::Transcribing));
        pause(400).await;
        shell.display(&Shown::Asked(question.into()));
        shell.update(&Update::State(TurnState::Thinking));
        if let Some(progress) = progress {
            shell.update(&Update::Progress(progress));
        }
        pause(2200).await;
        shell.update(&Update::State(TurnState::Speaking));
        for sentence in sentences {
            shell.display(&Shown::Said((*sentence).into()));
            pause(1800).await;
        }
        shell.update(&Update::State(TurnState::Idle));
        // The overlay fades out 6 s after a turn; start the next one first
        // so both rounds can be compared.
        pause(3000).await;
    }
    // The indicator's looks, one after another, named in the overlay.
    shell.display(&Shown::Clear);
    shell.update(&Update::State(TurnState::Speaking));
    for (caption, look) in glance_macos::glow::demo_looks() {
        shell.display(&Shown::Clear);
        shell.display(&Shown::Said(format!("Screenshot indicator: {caption}")));
        glance_macos::glow::preview(None, look);
        pause(1500).await;
    }
    shell.update(&Update::State(TurnState::Idle));
}

/// Orchestrator updates to the shell, plus the per-turn log line.
async fn forward_updates(mut updates: UnboundedReceiver<Update>, shell: Arc<dyn Shell>) {
    while let Some(update) = updates.recv().await {
        match &update {
            Update::Report(report) => {
                log_report(report);
                test_report(report);
            }
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
