//! The chords as Carbon hotkeys (`global-hotkey`). Carbon hotkeys do not
//! observe other keystrokes, so no Input Monitoring permission is needed
//! (`docs/DESIGN.md` §10). The manager must be created on the main thread,
//! and events arrive there through the app's run loop.
//!
//! Chords come from `[hotkeys]` in the config file, written like
//! "Ctrl+Opt+T" (modifiers Ctrl, Opt/Option/Alt, Shift, Cmd; then one key).
//! At least one modifier is required, so a chord can never swallow typing.

use std::sync::OnceLock;

use glance_orchestrator::{Chord, HotkeyEvent};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::HotkeysConfig;

/// The chords as the user wrote them, for the tray and log lines.
#[derive(Debug, Clone)]
pub struct Labels {
    pub talk: String,
    pub look: String,
    pub think: String,
}

static LABELS: OnceLock<Labels> = OnceLock::new();

/// The registered chords' labels (the defaults until registration).
pub fn labels() -> Labels {
    LABELS.get().cloned().unwrap_or_else(|| {
        let d = HotkeysConfig::default();
        Labels {
            talk: d.talk,
            look: d.look,
            think: d.think,
        }
    })
}

/// Parse "Ctrl+Opt+T". `global-hotkey` reads "Option" and "Alt" but not
/// "Opt", so that is translated first.
pub fn parse(chord: &str) -> Result<HotKey, String> {
    let normalized: Vec<String> = chord
        .split('+')
        .map(|t| match t.trim().to_ascii_lowercase().as_str() {
            "opt" => "Option".to_owned(),
            _ => t.trim().to_owned(),
        })
        .collect();
    if normalized.len() < 2 {
        return Err(format!(
            "`{chord}` needs at least one modifier (Ctrl, Opt, Shift or Cmd) and a key"
        ));
    }
    normalized
        .join("+")
        .parse::<HotKey>()
        .map_err(|e| format!("`{chord}` is not a chord: {e}"))
}

/// Keeps the chords registered for as long as it is alive.
pub struct Hotkeys {
    _manager: GlobalHotKeyManager,
}

/// Register the chords and forward their presses and releases to `events`.
/// On failure the message names every chord that could not be registered.
pub fn register(
    events: UnboundedSender<HotkeyEvent>,
    config: &HotkeysConfig,
) -> Result<Hotkeys, String> {
    let chords = [
        (Chord::Talk, config.talk.as_str()),
        (Chord::Look, config.look.as_str()),
        (Chord::Think, config.think.as_str()),
    ];
    let mut parsed = Vec::with_capacity(chords.len());
    for (chord, text) in chords {
        let hotkey = parse(text).map_err(|e| format!("hotkeys.{}: {e}", chord.as_str()))?;
        if let Some((other, _)) = parsed
            .iter()
            .find(|(_, h): &&(Chord, HotKey)| h.id() == hotkey.id())
        {
            return Err(format!(
                "hotkeys.{} and hotkeys.{} are the same chord ({text})",
                other.as_str(),
                chord.as_str()
            ));
        }
        parsed.push((chord, hotkey));
    }

    let manager =
        GlobalHotKeyManager::new().map_err(|e| format!("could not set up global hotkeys: {e}"))?;
    let failures: Vec<String> = parsed
        .iter()
        .zip(chords)
        .filter_map(|((_, hotkey), (_, label))| {
            manager
                .register(*hotkey)
                .err()
                .map(|e| format!("{label} could not be registered ({e})"))
        })
        .collect();
    if !failures.is_empty() {
        return Err(failures.join("; "));
    }
    let _ = LABELS.set(Labels {
        talk: config.talk.clone(),
        look: config.look.clone(),
        think: config.think.clone(),
    });

    let ids: Vec<(u32, Chord)> = parsed.iter().map(|(c, h)| (h.id(), *c)).collect();
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        let Some(&(_, chord)) = ids.iter().find(|(id, _)| *id == event.id()) else {
            return;
        };
        let event = match event.state() {
            HotKeyState::Pressed => HotkeyEvent::Pressed(chord),
            HotKeyState::Released => HotkeyEvent::Released(chord),
        };
        let _ = events.send(event);
    }));
    Ok(Hotkeys { _manager: manager })
}

#[cfg(test)]
mod tests {
    use global_hotkey::hotkey::{Code, Modifiers};

    use super::*;

    #[test]
    fn chords_parse_the_way_they_are_written_in_the_menu() {
        let think = parse("Ctrl+Opt+T").unwrap();
        assert_eq!(
            think,
            HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyT)
        );
        assert_eq!(parse("ctrl+option+shift+g").unwrap().key, Code::KeyG);
        assert_eq!(parse("Cmd+Opt+9").unwrap().key, Code::Digit9);
    }

    #[test]
    fn a_bare_key_or_nonsense_is_refused() {
        assert!(parse("T").is_err());
        assert!(parse("Ctrl+Opt").is_err());
        assert!(parse("Ctrl+Opt+Banana").is_err());
        assert!(parse("").is_err());
    }
}
