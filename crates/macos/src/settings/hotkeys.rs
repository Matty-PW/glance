//! Hotkeys: the three chords as keycaps; "Change…" opens an editor. A new
//! set applies only if every chord registers; otherwise the old chords stay
//! and the page says why (`hotkeys::rebind`).

use glance_orchestrator::settings::{
    SettingsBackend, SettingsChange, SettingsChoices, SettingsValues,
};
use objc2::rc::Retained;
use objc2::{MainThreadMarker, Message};
use objc2_app_kit::{NSButton, NSStackView, NSTextField, NSView};
use objc2_foundation::NSString;

use super::ui::{self, Page, Ui};
use super::{Reply, Section};
use crate::hotkeys::{self, Chords};

const NAMES: [(&str, &str); 3] = [
    ("Ask", "Hold, speak, release: a question about the screen."),
    ("Fresh look", "Like Ask, with a new screenshot."),
    (
        "Think harder",
        "Tap: ask DEEP the last question again. Hold: ask DEEP something new.",
    ),
];

const CHANGE: isize = 1;
const APPLY: isize = 2;
const CANCEL: isize = 3;

pub struct Hotkeys {
    page: Page,
    mtm: MainThreadMarker,
    caps: [Retained<NSStackView>; 3],
    fields: [Retained<NSTextField>; 3],
    editor: Retained<NSView>,
    change: Retained<NSButton>,
    current: [String; 3],
}

impl Hotkeys {
    pub fn new(ui: &Ui<'_>) -> Self {
        let mtm = ui.mtm;
        let page = Page::new(mtm);
        let caps = [0, 1, 2].map(|_| ui::stack(mtm, &[], false, 0.0));
        let rows: Vec<Retained<NSView>> = NAMES
            .iter()
            .zip(&caps)
            .map(|((name, detail), caps)| ui::row(mtm, name, Some(detail), false, caps))
            .collect();
        page.group(mtm, "Chords", &rows);
        let change = ui.button("Change…", CHANGE, false);
        page.add(&change_row(mtm, &change));

        let fields = [0, 1, 2].map(|_| ui.text_field("e.g. Ctrl+Opt+G", 0, 200.0));
        let mut edit_rows: Vec<Retained<NSView>> = NAMES
            .iter()
            .zip(&fields)
            .map(|((name, _), field)| ui::row(mtm, name, None, false, field))
            .collect();
        let apply = ui.button("Apply hotkeys", APPLY, true);
        let cancel = ui.button("Cancel", CANCEL, false);
        let buttons = ui::stack(mtm, &[&cancel, &apply], false, 8.0);
        edit_rows.push(ui::row(
            mtm,
            "",
            Some("Modifiers Ctrl, Opt, Shift, Cmd, then one key, joined with +."),
            false,
            &buttons,
        ));
        let before = page.stack.arrangedSubviews().count();
        page.group(mtm, "Change hotkeys", &edit_rows);
        let editor = page.stack.arrangedSubviews().objectAtIndex(before).retain();
        editor.setHidden(true);
        Self {
            page,
            mtm,
            caps,
            fields,
            editor,
            change,
            current: Default::default(),
        }
    }

    fn show_caps(&self) {
        for (caps, chord) in self.caps.iter().zip(&self.current) {
            for old in caps.arrangedSubviews().iter() {
                caps.removeArrangedSubview(&old);
                old.removeFromSuperview();
            }
            caps.addArrangedSubview(&ui::keycaps(self.mtm, chord));
        }
    }

    fn editing(&self, on: bool) {
        self.editor.setHidden(!on);
        self.change.setHidden(on);
        if on {
            for (field, chord) in self.fields.iter().zip(&self.current) {
                field.setStringValue(&NSString::from_str(chord));
            }
        }
    }

    fn apply(&mut self, chords: Chords, backend: &dyn SettingsBackend) -> Reply {
        match hotkeys::rebind(&chords) {
            Ok(()) => {
                let result = backend.apply(SettingsChange::Hotkeys([
                    chords.talk.clone(),
                    chords.look.clone(),
                    chords.think.clone(),
                ]));
                if result.is_ok() {
                    self.current = [chords.talk, chords.look, chords.think];
                    self.show_caps();
                    self.editing(false);
                }
                Reply::Applied(result)
            }
            // The old chords stay registered and shown.
            Err(e) => Reply::Say(e),
        }
    }
}

fn change_row(mtm: MainThreadMarker, change: &NSButton) -> Retained<NSView> {
    let spacer = ui::label(mtm, "");
    ui::row_with(mtm, &spacer, change)
}

impl Section for Hotkeys {
    fn name(&self) -> &'static str {
        "Hotkeys"
    }

    fn symbol(&self) -> &'static str {
        "keyboard"
    }

    fn view(&self) -> Retained<NSView> {
        Retained::into_super(self.page.root.clone())
    }

    fn fill(&mut self, values: &SettingsValues, _choices: &SettingsChoices) {
        // What is registered now, when the chords are live.
        self.current = if hotkeys::is_active() {
            let labels = hotkeys::labels();
            [labels.talk, labels.look, labels.think]
        } else {
            values.hotkeys.clone()
        };
        self.show_caps();
        self.editing(false);
    }

    fn on_change(&mut self, id: isize, backend: &dyn SettingsBackend) -> Reply {
        match id {
            CHANGE => {
                self.editing(true);
                Reply::Nothing
            }
            CANCEL => {
                self.editing(false);
                Reply::Nothing
            }
            APPLY => {
                let text = |i: usize| self.fields[i].stringValue().to_string().trim().to_owned();
                let chords = Chords {
                    talk: text(0),
                    look: text(1),
                    think: text(2),
                };
                self.apply(chords, backend)
            }
            _ => Reply::Nothing,
        }
    }

    fn restore(&mut self, d: &SettingsValues, backend: &dyn SettingsBackend) -> Option<Reply> {
        let [talk, look, think] = d.hotkeys.clone();
        Some(self.apply(Chords { talk, look, think }, backend))
    }

    fn restore_note(&self) -> &'static str {
        "The chords go back to Ctrl+Opt+G, Ctrl+Opt+Shift+G and Ctrl+Opt+T, if they can be registered."
    }
}
