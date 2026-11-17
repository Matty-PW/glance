//! The menu-bar icon and its menu: a status line, the permission checklist
//! with links into System Settings, and Quit.
//!
//! AppKit objects may only be used on the main thread, so all tray state
//! lives in a thread-local, and every function here is called on the main
//! thread (directly, or through the main dispatch queue).

use std::cell::RefCell;

use dispatch2::DispatchQueue;
use glance_audio::permission::{MicPermission, microphone_permission};
use glance_capture::live::screen_recording_granted;
use glance_orchestrator::TurnState;
use objc2::MainThreadMarker;
use objc2_app_kit::NSApplication;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use super::hotkeys::{LOOK_LABEL, TALK_LABEL};

const SCREEN_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";
const MIC_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone";

const ID_SCREEN: &str = "permission-screen";
const ID_MIC: &str = "permission-mic";
const ID_QUIT: &str = "quit";

/// A change to show. Built on any thread, applied on the main thread.
pub enum View {
    State(TurnState),
    Status(String),
    /// Something that stops glance working (a chord or a model failed to
    /// load). Stays until quit.
    Fatal(String),
    /// The last turn failed; shown beside "Ready" until the next press.
    TurnFailed(String),
}

impl View {
    pub fn state(state: TurnState) -> Self {
        View::State(state)
    }

    pub fn status(text: impl Into<String>) -> Self {
        View::Status(text.into())
    }

    pub fn error(text: impl Into<String>) -> Self {
        View::Fatal(text.into())
    }

    pub fn turn_failed(text: impl Into<String>) -> Self {
        View::TurnFailed(text.into())
    }
}

struct Tray {
    icon: TrayIcon,
    status: MenuItem,
    screen: MenuItem,
    mic: MenuItem,
}

struct Ui {
    tray: Option<Tray>,
    status: String,
    /// Short marker next to the icon, so state is visible without opening
    /// the menu (there is no overlay until M5).
    title: &'static str,
    fatal: bool,
    problem: Option<String>,
}

thread_local! {
    static UI: RefCell<Ui> = const {
        RefCell::new(Ui {
            tray: None,
            status: String::new(),
            title: "",
            fatal: false,
            problem: None,
        })
    };
}

/// Create the icon and menu. Must run after the app's run loop has started.
pub fn install(model_line: &str) {
    let status = MenuItem::new("Starting…", false, None);
    let screen = MenuItem::with_id(ID_SCREEN, "Screen Recording: …", false, None);
    let mic = MenuItem::with_id(ID_MIC, "Microphone: …", false, None);
    let quit = MenuItem::with_id(ID_QUIT, "Quit glance", true, None);
    let menu = Menu::new();
    let built = menu.append_items(&[
        &status,
        &MenuItem::new(model_line, false, None),
        &MenuItem::new(
            format!("Hold {TALK_LABEL} or {LOOK_LABEL} and ask"),
            false,
            None,
        ),
        &PredefinedMenuItem::separator(),
        &MenuItem::new("Permissions", false, None),
        &screen,
        &mic,
        &PredefinedMenuItem::separator(),
        &quit,
    ]);
    if let Err(e) = built {
        eprintln!("glance: error: could not build the tray menu: {e}");
        return;
    }
    let icon = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon_templated(icon())
        .with_tooltip("glance")
        .build();
    let icon = match icon {
        Ok(icon) => icon,
        Err(e) => {
            eprintln!("glance: error: could not create the menu-bar icon: {e}");
            return;
        }
    };
    MenuEvent::set_event_handler(Some(|event: MenuEvent| on_menu(event.id.0.as_str())));

    UI.with_borrow_mut(|ui| {
        ui.tray = Some(Tray {
            icon,
            status,
            screen,
            mic,
        });
        ui.render();
    });
    refresh_permissions();
}

/// Apply a change.
pub fn apply(view: View) {
    UI.with_borrow_mut(|ui| {
        ui.update(view);
        ui.render();
    });
}

/// Re-read both permissions and update the checklist.
pub fn refresh_permissions() {
    UI.with_borrow(|ui| {
        let Some(tray) = &ui.tray else { return };
        if screen_recording_granted() {
            tray.screen.set_text("Screen Recording: allowed");
            tray.screen.set_enabled(false);
        } else {
            tray.screen.set_text(
                "Screen Recording: not allowed (relaunch after allowing). Open Settings…",
            );
            tray.screen.set_enabled(true);
        }
        let (text, open) = match microphone_permission() {
            MicPermission::Granted => ("Microphone: allowed", false),
            MicPermission::NotAsked => ("Microphone: macOS asks at the first press", false),
            MicPermission::Denied | MicPermission::Restricted => {
                ("Microphone: not allowed. Open Settings…", true)
            }
            MicPermission::Unknown => ("Microphone: unknown. Open Settings…", true),
        };
        tray.mic.set_text(text);
        tray.mic.set_enabled(open);
    });
}

impl Ui {
    fn update(&mut self, view: View) {
        if self.fatal {
            return;
        }
        match view {
            View::Fatal(message) => {
                self.fatal = true;
                self.status = message;
                self.title = "!";
            }
            View::Status(text) => {
                self.status = text;
                self.title = "";
            }
            View::TurnFailed(problem) => self.problem = Some(problem),
            View::State(state) => {
                if state == TurnState::Listening {
                    self.problem = None;
                }
                let (status, title) = match state {
                    TurnState::Idle => ("Ready", ""),
                    TurnState::Listening => ("Listening…", "●"),
                    TurnState::Transcribing => ("Transcribing…", "…"),
                    TurnState::Thinking => ("Thinking…", "…"),
                    TurnState::Speaking => ("Speaking…", "▸"),
                };
                self.status = match (&self.problem, state) {
                    (Some(problem), TurnState::Idle) => format!("Ready (last turn: {problem})"),
                    _ => status.to_owned(),
                };
                self.title = title;
            }
        }
    }

    fn render(&self) {
        let Some(tray) = &self.tray else { return };
        tray.status.set_text(&self.status);
        tray.icon
            .set_title((!self.title.is_empty()).then_some(self.title));
        let _ = tray
            .icon
            .set_tooltip(Some(format!("glance: {}", self.status)));
    }
}

fn on_menu(id: &str) {
    match id {
        ID_SCREEN => open_settings(SCREEN_SETTINGS),
        ID_MIC => open_settings(MIC_SETTINGS),
        ID_QUIT => DispatchQueue::main().exec_async(|| {
            if let Some(mtm) = MainThreadMarker::new() {
                NSApplication::sharedApplication(mtm).terminate(None);
            }
        }),
        _ => {}
    }
}

/// Open a System Settings pane. `open` with a fixed URL; nothing from the
/// screen or the model ever reaches this.
fn open_settings(url: &'static str) {
    if let Err(e) = std::process::Command::new("/usr/bin/open").arg(url).spawn() {
        eprintln!("glance: could not open System Settings: {e}");
    }
}

/// A 36x36 template image (18 pt at 2x): a ring with a centre dot, drawn in
/// code so there is no image file. macOS tints template images to suit the
/// menu bar.
fn icon() -> Icon {
    const SIZE: u32 = 36;
    let centre = (SIZE as f32 - 1.0) / 2.0;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let d = ((x as f32 - centre).powi(2) + (y as f32 - centre).powi(2)).sqrt();
            let alpha = band(d, 11.5, 15.5).max(band(d, -1.0, 5.5));
            rgba.extend_from_slice(&[0, 0, 0, (alpha * 255.0).round() as u8]);
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).expect("the icon buffer matches its size")
}

/// Anti-aliased coverage of a pixel at distance `d` inside the band
/// `inner..outer`.
fn band(d: f32, inner: f32, outer: f32) -> f32 {
    (d - inner + 0.5)
        .clamp(0.0, 1.0)
        .min((outer - d + 0.5).clamp(0.0, 1.0))
}
