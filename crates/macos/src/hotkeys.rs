//! The chords as Carbon hotkeys (`global-hotkey`). Carbon hotkeys do not
//! observe other keystrokes, so no Input Monitoring permission is needed
//! (`docs/DESIGN.md` §10). The manager must be created on the main thread,
//! and events arrive there through the app's run loop.
//!
//! Chords come from `[hotkeys]` in the app's config file, written like
//! "Ctrl+Opt+T" (modifiers Ctrl, Opt/Option/Alt, Shift, Cmd; then one key).
//! At least one modifier is required, so a chord can never swallow typing.

use std::cell::RefCell;
use std::sync::RwLock;

use glance_orchestrator::{Chord, HotkeyEvent};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tokio::sync::mpsc::UnboundedSender;

/// The three chords as written, e.g. "Ctrl+Opt+T".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chords {
    pub talk: String,
    pub look: String,
    pub think: String,
}

impl Default for Chords {
    fn default() -> Self {
        Self {
            talk: "Ctrl+Opt+G".into(),
            look: "Ctrl+Opt+Shift+G".into(),
            think: "Ctrl+Opt+T".into(),
        }
    }
}

/// The chords as the user wrote them, for the tray and log lines.
#[derive(Debug, Clone)]
pub struct Labels {
    pub talk: String,
    pub look: String,
    pub think: String,
}

static LABELS: RwLock<Option<Labels>> = RwLock::new(None);
/// Which registered hotkey id is which chord (read by the event handler).
static IDS: RwLock<Vec<(u32, Chord)>> = RwLock::new(Vec::new());

/// The manager and the chords now registered.
type Active = Option<(GlobalHotKeyManager, Vec<(Chord, HotKey)>)>;

thread_local! {
    /// Main thread only.
    static ACTIVE: RefCell<Active> = const { RefCell::new(None) };
}

/// The registered chords' labels (the defaults until registration).
pub fn labels() -> Labels {
    LABELS
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .unwrap_or_else(|| {
            let d = Chords::default();
            Labels {
                talk: d.talk,
                look: d.look,
                think: d.think,
            }
        })
}

fn set_labels(chords: &Chords) {
    *LABELS.write().unwrap_or_else(|p| p.into_inner()) = Some(Labels {
        talk: chords.talk.clone(),
        look: chords.look.clone(),
        think: chords.think.clone(),
    });
}

fn set_ids(registered: &[(Chord, HotKey)]) {
    *IDS.write().unwrap_or_else(|p| p.into_inner()) =
        registered.iter().map(|(c, h)| (h.id(), *c)).collect();
}

/// Parse all three chords and refuse duplicates.
fn parse_all(chords: &Chords) -> Result<Vec<(Chord, HotKey, String)>, String> {
    let wanted = [
        (Chord::Talk, &chords.talk),
        (Chord::Look, &chords.look),
        (Chord::Think, &chords.think),
    ];
    let mut parsed: Vec<(Chord, HotKey, String)> = Vec::with_capacity(3);
    for (chord, text) in wanted {
        let hotkey = parse(text).map_err(|e| format!("hotkeys.{}: {e}", chord.as_str()))?;
        if let Some((other, _, _)) = parsed.iter().find(|(_, h, _)| h.id() == hotkey.id()) {
            return Err(format!(
                "hotkeys.{} and hotkeys.{} are the same chord ({text})",
                other.as_str(),
                chord.as_str()
            ));
        }
        parsed.push((chord, hotkey, text.clone()));
    }
    Ok(parsed)
}

/// Change the chords while running (settings window, main thread). Each new
/// chord is registered before the old one is released; if any cannot be
/// registered, every change is undone and the old chords stay.
pub fn rebind(chords: &Chords) -> Result<(), String> {
    let wanted = parse_all(chords)?;
    ACTIVE.with_borrow_mut(|active| {
        let Some((manager, current)) = active.as_mut() else {
            return Err("hotkeys are not set up".to_owned());
        };
        let changed: Vec<&(Chord, HotKey, String)> = wanted
            .iter()
            .filter(|(chord, hotkey, _)| {
                current
                    .iter()
                    .find(|(c, _)| c == chord)
                    .is_none_or(|(_, h)| h.id() != hotkey.id())
            })
            .collect();
        let mut added = Vec::new();
        for (chord, hotkey, text) in &changed {
            if let Err(e) = manager.register(*hotkey) {
                for undo in added {
                    let _ = manager.unregister(undo);
                }
                return Err(format!(
                    "{text} could not be registered for {} ({e}); the old chord stays",
                    chord.as_str()
                ));
            }
            added.push(*hotkey);
        }
        for (chord, _, _) in &changed {
            if let Some((_, old)) = current.iter().find(|(c, _)| c == chord) {
                let _ = manager.unregister(*old);
            }
        }
        *current = wanted.iter().map(|(c, h, _)| (*c, *h)).collect();
        set_ids(current);
        set_labels(chords);
        Ok(())
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

/// Proof that the chords are registered; the manager itself lives in
/// main-thread state so the settings window can change the chords.
pub struct Hotkeys {
    _private: (),
}

/// Register the chords and forward their presses and releases to `events`.
/// On failure the message names every chord that could not be registered.
pub fn register(events: UnboundedSender<HotkeyEvent>, config: &Chords) -> Result<Hotkeys, String> {
    let parsed = parse_all(config)?;
    let manager =
        GlobalHotKeyManager::new().map_err(|e| format!("could not set up global hotkeys: {e}"))?;
    let failures: Vec<String> = parsed
        .iter()
        .filter_map(|(_, hotkey, label)| {
            manager
                .register(*hotkey)
                .err()
                .map(|e| format!("{label} could not be registered ({e})"))
        })
        .collect();
    if !failures.is_empty() {
        return Err(failures.join("; "));
    }
    let registered: Vec<(Chord, HotKey)> = parsed.iter().map(|(c, h, _)| (*c, *h)).collect();
    set_ids(&registered);
    set_labels(config);
    ACTIVE.with_borrow_mut(|active| *active = Some((manager, registered)));

    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        let chord = IDS
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .find(|(id, _)| *id == event.id())
            .map(|(_, chord)| *chord);
        let Some(chord) = chord else { return };
        let event = match event.state() {
            HotKeyState::Pressed => HotkeyEvent::Pressed(chord),
            HotKeyState::Released => HotkeyEvent::Released(chord),
        };
        let _ = events.send(event);
    }));
    Ok(Hotkeys { _private: () })
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
