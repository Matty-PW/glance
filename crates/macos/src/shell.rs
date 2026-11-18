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
}

struct Actions {
    on_quit: Box<dyn Fn() + Send + Sync>,
    on_new_session: Box<dyn Fn() + Send + Sync>,
}

static ACTIONS: OnceLock<Actions> = OnceLock::new();

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
