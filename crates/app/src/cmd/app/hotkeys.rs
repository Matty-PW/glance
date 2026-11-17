//! The two chords as Carbon hotkeys (`global-hotkey`). Carbon hotkeys do not
//! observe other keystrokes, so no Input Monitoring permission is needed
//! (`docs/DESIGN.md` §10). The manager must be created on the main thread,
//! and events arrive there through the app's run loop.

use glance_orchestrator::{Chord, HotkeyEvent};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tokio::sync::mpsc::UnboundedSender;

pub const TALK_LABEL: &str = "Ctrl+Opt+G";
pub const LOOK_LABEL: &str = "Ctrl+Opt+Shift+G";

/// Keeps the chords registered for as long as it is alive.
pub struct Hotkeys {
    _manager: GlobalHotKeyManager,
}

/// Register both chords and forward their presses and releases to `events`.
/// On failure the message names every chord that could not be registered.
pub fn register(events: UnboundedSender<HotkeyEvent>) -> Result<Hotkeys, String> {
    let manager =
        GlobalHotKeyManager::new().map_err(|e| format!("could not set up global hotkeys: {e}"))?;
    let talk = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyG);
    let look = HotKey::new(
        Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT),
        Code::KeyG,
    );

    let failures: Vec<String> = [(talk, TALK_LABEL), (look, LOOK_LABEL)]
        .into_iter()
        .filter_map(|(hotkey, label)| {
            manager
                .register(hotkey)
                .err()
                .map(|e| format!("{label} could not be registered ({e})"))
        })
        .collect();
    if !failures.is_empty() {
        return Err(failures.join("; "));
    }

    let (talk_id, look_id) = (talk.id(), look.id());
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        let chord = match event.id() {
            id if id == talk_id => Chord::Talk,
            id if id == look_id => Chord::Look,
            _ => return,
        };
        let event = match event.state() {
            HotKeyState::Pressed => HotkeyEvent::Pressed(chord),
            HotKeyState::Released => HotkeyEvent::Released(chord),
        };
        let _ = events.send(event);
    }));
    Ok(Hotkeys { _manager: manager })
}
