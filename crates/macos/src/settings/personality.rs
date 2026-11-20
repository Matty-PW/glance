//! Personality: how answers sound (never how long).

use glance_orchestrator::settings::{
    SettingsBackend, SettingsChange, SettingsChoices, SettingsValues,
};
use objc2::rc::Retained;
use objc2_app_kit::{NSPopUpButton, NSTextField, NSView};
use objc2_foundation::NSString;

use super::ui::{self, Page, Ui};
use super::{Reply, Section};

/// Preset names as saved, and as shown.
const PRESETS: [(&str, &str); 8] = [
    ("neutral", "Neutral (as before)"),
    ("friendly", "Friendly"),
    ("direct", "Direct"),
    ("strict", "Strict"),
    ("pirate", "Pirate (fun)"),
    ("caveman", "Caveman (fun)"),
    ("shakespearean", "Shakespearean (fun)"),
    ("custom", "Custom"),
];

const PRESET: isize = 1;
const CUSTOM: isize = 2;

pub struct Personality {
    page: Page,
    preset: Retained<NSPopUpButton>,
    custom: Retained<NSTextField>,
}

impl Personality {
    pub fn new(ui: &Ui<'_>) -> Self {
        let mtm = ui.mtm;
        let page = Page::new(mtm);
        let preset = ui.popup(&PRESETS.map(|(_, l)| l.to_owned()), PRESET);
        let custom = ui.text_field("e.g. Calm and patient, like a good teacher", CUSTOM, ui::CONTROL_W);
        page.group(
            mtm,
            "Style",
            &[
                ui::row(
                    mtm,
                    "Personality",
                    Some("How answers sound, never how long. Fun ones play a character."),
                    false,
                    &preset,
                ),
                ui::row(
                    mtm,
                    "Custom style",
                    Some("For Custom: up to 300 characters, then press Return. Naming the words to use works best."),
                    false,
                    &custom,
                ),
            ],
        );
        Self {
            page,
            preset,
            custom,
        }
    }
}

impl Section for Personality {
    fn name(&self) -> &'static str {
        "Personality"
    }

    fn symbol(&self) -> &'static str {
        "theatermasks"
    }

    fn view(&self) -> Retained<NSView> {
        Retained::into_super(self.page.root.clone())
    }

    fn fill(&mut self, values: &SettingsValues, _choices: &SettingsChoices) {
        let at = PRESETS
            .iter()
            .position(|(p, _)| *p == values.personality)
            .unwrap_or(0);
        self.preset.selectItemAtIndex(at as isize);
        self.custom
            .setStringValue(&NSString::from_str(&values.personality_custom));
        self.custom.setEnabled(values.personality == "custom");
    }

    fn on_change(&mut self, id: isize, backend: &dyn SettingsBackend) -> Reply {
        if id != PRESET && id != CUSTOM {
            return Reply::Nothing;
        }
        let preset = PRESETS[ui::index(&self.preset, PRESETS.len())].0;
        let custom = self.custom.stringValue().to_string();
        self.custom.setEnabled(preset == "custom");
        if preset == "custom" && custom.trim().is_empty() {
            return Reply::Say("Type a style for Custom, then press Return.".into());
        }
        Reply::Applied(backend.apply(SettingsChange::Personality {
            preset: preset.to_owned(),
            custom,
        }))
    }

    fn restore(&mut self, d: &SettingsValues, backend: &dyn SettingsBackend) -> Option<Reply> {
        Some(Reply::Applied(backend.apply(SettingsChange::Personality {
            preset: d.personality.clone(),
            custom: d.personality_custom.clone(),
        })))
    }

    fn restore_note(&self) -> &'static str {
        "Personality goes back to neutral and the custom style is cleared."
    }
}
