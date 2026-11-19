//! The app on macOS: `NSApplication` with the Accessory policy (no Dock
//! icon) on the main thread, the Carbon hotkeys, the menu-bar icon, and
//! [`MacShell`], the [`Shell`] the app's portable code talks to.
//!
//! The main run loop delivers hotkey events, tray clicks and the system
//! voice's callbacks. AppKit objects may only be touched on the main thread,
//! so every UI change goes through the main dispatch queue.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use dispatch2::DispatchQueue;
use glance_orchestrator::settings::{ModelsBackend, SettingsBackend};
use glance_orchestrator::{HotkeyEvent, Outcome, Shell, Shown, Update};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::hotkeys::{self, Chords};
use crate::overlay::{self, OverlayEvent};
use crate::permission::{request_screen_recording, screen_recording_granted};
use crate::tray::{self, View};

/// What the app hands the shell.
pub struct Options {
    pub chords: Chords,
    /// "FAST: … · DEEP: …" for the menu.
    pub model_line: String,
    /// Runs on Quit, before the app terminates (`terminate` skips
    /// destructors): stop the servers glance started.
    pub on_quit: Box<dyn Fn() + Send + Sync>,
    /// The tray's "New session".
    pub on_new_session: Box<dyn Fn() + Send + Sync>,
    /// Show the floating overlay (config `[overlay] enabled`).
    pub overlay: bool,
    /// Reads and saves settings for the settings window.
    pub settings: Option<Arc<dyn SettingsBackend>>,
    /// Lists, downloads, checks and deletes models for the models window.
    pub models: Option<Arc<dyn ModelsBackend>>,
}

struct Actions {
    on_quit: Box<dyn Fn() + Send + Sync>,
    on_new_session: Box<dyn Fn() + Send + Sync>,
}

static ACTIONS: OnceLock<Actions> = OnceLock::new();
static SETTINGS: OnceLock<Arc<dyn SettingsBackend>> = OnceLock::new();
static MODELS: OnceLock<Arc<dyn ModelsBackend>> = OnceLock::new();

/// The app's models backend, if it gave one.
pub(crate) fn models_backend() -> Option<Arc<dyn ModelsBackend>> {
    MODELS.get().cloned()
}

/// The app's settings backend, if it gave one.
pub(crate) fn settings_backend() -> Option<Arc<dyn SettingsBackend>> {
    SETTINGS.get().cloned()
}

/// The [`Shell`] on macOS: the menu-bar icon and the overlay.
pub struct MacShell {
    overlay: bool,
}

impl Shell for MacShell {
    fn status(&self, text: &str) {
        show(View::status(text));
    }

    fn fatal(&self, text: &str) {
        show(View::error(text));
    }

    fn update(&self, update: &Update) {
        if self.overlay {
            let event = match update {
                Update::State(state) => Some(OverlayEvent::State(*state)),
                Update::Progress(progress) => Some(OverlayEvent::Progress(*progress)),
                Update::Deep(status) => Some(OverlayEvent::Deep(*status)),
                Update::Session(info) => Some(OverlayEvent::Session(*info)),
                Update::Report(_) => None,
            };
            if let Some(event) = event {
                DispatchQueue::main().exec_async(move || overlay::apply(event));
            }
        }
        match update {
            Update::State(state) => show(View::state(*state)),
            Update::Report(report) => {
                if let Outcome::Failed(error) = &report.outcome {
                    show(View::turn_failed(error.to_string()));
                }
            }
            Update::Progress(progress) => show(View::progress(*progress)),
            Update::Deep(status) => show(View::deep(*status)),
            Update::Session(info) => show(View::session(*info)),
        }
    }

    fn display(&self, shown: &Shown) {
        if self.overlay {
            let event = OverlayEvent::Shown(shown.clone());
            DispatchQueue::main().exec_async(move || overlay::apply(event));
        }
    }
}

/// Run the app until Quit. `ready` is called once, on the main thread
/// before the run loop starts, with the hotkey events (`None` if a chord
/// could not be registered: then glance shows the error and does nothing
/// else) and the shell.
pub fn run(
    options: Options,
    ready: impl FnOnce(Option<UnboundedReceiver<HotkeyEvent>>, Arc<dyn Shell>),
) -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("glance app must run on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let _ = ACTIONS.set(Actions {
        on_quit: options.on_quit,
        on_new_session: options.on_new_session,
    });
    if let Some(settings) = options.settings {
        let _ = SETTINGS.set(settings);
    }
    if let Some(models) = options.models {
        let _ = MODELS.set(models);
    }

    // Hotkeys first: if a chord cannot be registered, say which and do not
    // run the pipeline, so glance never sits there without a working chord.
    let (keys, key_events) = unbounded_channel();
    let registered = hotkeys::register(keys, &options.chords);
    // The tray may only be created once the run loop is running; the main
    // queue runs this block as soon as it is.
    let model_line = options.model_line.clone();
    let with_overlay = options.overlay;
    DispatchQueue::main().exec_async(move || {
        tray::install(&model_line);
        if with_overlay {
            overlay::install();
        }
    });

    let shell: Arc<dyn Shell> = Arc::new(MacShell {
        overlay: options.overlay,
    });
    let _hotkeys = match registered {
        Ok(hotkeys) => {
            let labels = hotkeys::labels();
            eprintln!(
                "glance: chords registered: {} (talk), {} (look), {} (think harder)",
                labels.talk, labels.look, labels.think
            );
            ready(Some(key_events), Arc::clone(&shell));
            Some(hotkeys)
        }
        Err(message) => {
            eprintln!("glance: error: {message}");
            shell.fatal(&format!("Hotkey error: {message}"));
            ready(None, Arc::clone(&shell));
            None
        }
    };

    // Screen Recording: ask at launch rather than at the first question.
    // macOS shows its prompt once; a grant needs a relaunch to take effect.
    if !screen_recording_granted() {
        eprintln!("glance: Screen Recording is not granted; asking (relaunch after granting)");
        request_screen_recording();
    }
    // Permissions can change in System Settings at any time, and the
    // screenshot's age moves on: refresh both every few seconds.
    std::thread::Builder::new()
        .name("glance-tray-refresh".into())
        .spawn(|| {
            loop {
                std::thread::sleep(Duration::from_secs(3));
                DispatchQueue::main().exec_async(|| {
                    tray::refresh();
                    overlay::tick();
                });
            }
        })
        .map_err(|e| format!("could not start the tray refresh: {e}"))?;

    eprintln!(
        "glance: running ({}). Quit from the menu-bar icon.",
        options.model_line
    );
    app.run();
    Ok(())
}

/// Quit (tray menu): the app's clean-up, then terminate.
pub(crate) fn quit() {
    if let Some(actions) = ACTIONS.get() {
        (actions.on_quit)();
    }
    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm).terminate(None);
    }
}

/// "Apply and restart" (settings): start a fresh glance with the same
/// arguments once this one has exited, so its hotkeys and model servers are
/// free. Inside Glance.app that goes through `open -n`.
pub(crate) fn relaunch() {
    let Ok(exe) = std::env::current_exe() else {
        eprintln!("glance: cannot find this executable to restart it; quit and start glance again");
        return;
    };
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let exe_text = exe.to_string_lossy().into_owned();
    let mut command = std::process::Command::new("/bin/sh");
    // The arguments go in as positional parameters, never into the script
    // text, so nothing in them is interpreted by the shell. The helper waits
    // for this process to exit (at most 30 s), then starts the new one.
    command.args([
        "-c",
        "p=$1; shift; i=0; while kill -0 \"$p\" 2>/dev/null && [ $i -lt 150 ]; do sleep 0.2; i=$((i+1)); done; exec \"$@\"",
        "sh",
        &std::process::id().to_string(),
    ]);
    match exe_text.find(".app/Contents/MacOS/") {
        Some(at) => {
            command.args(["/usr/bin/open", "-n", &exe_text[..at + 4], "--args"]);
        }
        None => {
            command.arg(&exe);
        }
    }
    command.args(&args);
    match command.spawn() {
        Ok(_) => quit(),
        Err(e) => eprintln!("glance: could not restart ({e}); quit and start glance again"),
    }
}

/// Show the settings window (tray menu, or `--open-settings`).
pub fn open_settings() {
    dispatch2::DispatchQueue::main().exec_async(crate::settings::open);
}

/// Show the models window (tray menu, or `--open-models`).
pub fn open_models() {
    dispatch2::DispatchQueue::main().exec_async(crate::models_window::open);
}

/// "New session" (tray menu).
pub(crate) fn new_session() {
    if let Some(actions) = ACTIONS.get() {
        (actions.on_new_session)();
    }
}

/// Apply a tray change on the main thread.
fn show(view: View) {
    DispatchQueue::main().exec_async(move || tray::apply(view));
}
