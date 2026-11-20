//! Voice: engine, voice, Kokoro speed, maths as words.

use glance_orchestrator::settings::{
    SettingsBackend, SettingsChange, SettingsChoices, SettingsValues,
};
use objc2::rc::Retained;
use objc2_app_kit::{
    NSControlStateValueOn, NSPopUpButton, NSSlider, NSSwitch, NSTextField, NSView,
};
use objc2_foundation::NSString;

use super::ui::{self, Page, Ui};
use super::{Reply, Section};

const ENGINES: [(&str, &str); 2] = [("kokoro", "Kokoro"), ("system", "macOS system voice")];

const ENGINE: isize = 1;
const VOICE: isize = 2;
const SPEED: isize = 3;
const MATHS: isize = 4;

pub struct Voice {
    page: Page,
    engine: Retained<NSPopUpButton>,
    voice: Retained<NSPopUpButton>,
    speed: Retained<NSSlider>,
    speed_label: Retained<NSTextField>,
    maths: Retained<NSSwitch>,
    choices: SettingsChoices,
    /// The voice for each engine, so switching engines keeps both.
    kokoro_voice: String,
    system_voice: Option<String>,
}

impl Voice {
    pub fn new(ui: &Ui<'_>) -> Self {
        let mtm = ui.mtm;
        let page = Page::new(mtm);
        let engine = ui.popup(&ENGINES.map(|(_, l)| l.to_owned()), ENGINE);
        let voice = ui.popup(&[], VOICE);
        let speed = ui.slider(0.5, 2.0, None, SPEED);
        let speed_label = ui.value_label(56.0);
        let speed_box: Retained<NSView> =
            Retained::into_super(ui::stack(mtm, &[&speed, &speed_label], false, 8.0));
        ui::activate(&[speed_box
            .widthAnchor()
            .constraintEqualToConstant(ui::CONTROL_W)]);
        let maths = ui.switch(MATHS);
        page.group(
            mtm,
            "Speech",
            &[
                ui::row(
                    mtm,
                    "Engine",
                    Some("Kokoro runs on this Mac; the system voice is the macOS one."),
                    true,
                    &engine,
                ),
                ui::row(mtm, "Voice", None, false, &voice),
                ui::row(mtm, "Speed", Some("Kokoro only."), false, &speed_box),
            ],
        );
        page.group(
            mtm,
            "Reading",
            &[ui::row(
                mtm,
                "Say maths symbols as words",
                Some("2 × 2 = 4 is spoken \u{201c}2 times 2 equals 4\u{201d}. The text on screen is unchanged."),
                false,
                &maths,
            )],
        );
        Self {
            page,
            engine,
            voice,
            speed,
            speed_label,
            maths,
            choices: SettingsChoices::default(),
            kokoro_voice: String::new(),
            system_voice: None,
        }
    }

    fn is_kokoro(&self) -> bool {
        ui::index(&self.engine, ENGINES.len()) == 0
    }

    /// The voice list for the selected engine, with its voice selected.
    fn fill_voices(&self) {
        if self.is_kokoro() {
            let mut items = self.choices.kokoro_voices.clone();
            // A configured blend ("af_heart:0.6,bf_emma:0.4") is kept as an item.
            if !items.contains(&self.kokoro_voice) {
                items.insert(0, self.kokoro_voice.clone());
            }
            ui::set_items(&self.voice, &items);
            ui::select(&self.voice, &items, &self.kokoro_voice);
        } else {
            let mut titles = vec!["System default".to_owned()];
            titles.extend(
                self.choices
                    .system_voices
                    .iter()
                    .map(|(_, label)| label.clone()),
            );
            ui::set_items(&self.voice, &titles);
            let at = self.system_voice.as_ref().and_then(|wanted| {
                self.choices
                    .system_voices
                    .iter()
                    .position(|(id, _)| id == wanted)
            });
            self.voice
                .selectItemAtIndex(at.map_or(0, |i| i as isize + 1));
        }
        self.speed.setEnabled(self.is_kokoro());
    }

    fn speed_value(&self) -> f32 {
        ((self.speed.doubleValue() * 20.0).round() / 20.0) as f32
    }

    fn show_speed(&self) {
        self.speed_label
            .setStringValue(&NSString::from_str(&format!("{:.2}×", self.speed_value())));
    }

    fn voice_change(&mut self) -> SettingsChange {
        let engine = ENGINES[ui::index(&self.engine, ENGINES.len())].0.to_owned();
        if self.is_kokoro() {
            if let Some(title) = self.voice.titleOfSelectedItem() {
                self.kokoro_voice = title.to_string();
            }
        } else {
            let i = self.voice.indexOfSelectedItem();
            self.system_voice = (i > 0)
                .then(|| self.choices.system_voices.get(i as usize - 1))
                .flatten()
                .map(|(id, _)| id.clone());
        }
        SettingsChange::Voice {
            engine,
            kokoro_voice: self.kokoro_voice.clone(),
            system_voice: self.system_voice.clone(),
            speed: self.speed_value(),
        }
    }
}

impl Section for Voice {
    fn name(&self) -> &'static str {
        "Voice"
    }

    fn symbol(&self) -> &'static str {
        "waveform"
    }

    fn view(&self) -> Retained<NSView> {
        Retained::into_super(self.page.root.clone())
    }

    fn fill(&mut self, values: &SettingsValues, choices: &SettingsChoices) {
        self.choices = choices.clone();
        self.kokoro_voice = values.kokoro_voice.clone();
        self.system_voice = values.system_voice.clone();
        let engine = ENGINES
            .iter()
            .position(|(e, _)| *e == values.engine)
            .unwrap_or(0);
        self.engine.selectItemAtIndex(engine as isize);
        self.fill_voices();
        self.speed.setDoubleValue(f64::from(values.speed));
        self.show_speed();
        self.maths.setState(ui::on_off(values.math_as_words));
    }

    fn on_change(&mut self, id: isize, backend: &dyn SettingsBackend) -> Reply {
        match id {
            ENGINE => {
                self.fill_voices();
                Reply::Applied(backend.apply(self.voice_change()))
            }
            VOICE | SPEED => {
                self.show_speed();
                Reply::Applied(backend.apply(self.voice_change()))
            }
            MATHS => Reply::Applied(backend.apply(SettingsChange::MathAsWords(
                self.maths.state() == NSControlStateValueOn,
            ))),
            _ => Reply::Nothing,
        }
    }

    fn restore(&mut self, d: &SettingsValues, backend: &dyn SettingsBackend) -> Option<Reply> {
        let voice = backend.apply(SettingsChange::Voice {
            engine: d.engine.clone(),
            kokoro_voice: d.kokoro_voice.clone(),
            system_voice: d.system_voice.clone(),
            speed: d.speed,
        });
        let maths = backend.apply(SettingsChange::MathAsWords(d.math_as_words));
        Some(Reply::Applied(voice.and(maths)))
    }

    fn restore_note(&self) -> &'static str {
        "Engine, voice, speed and maths go back to their defaults. Changing the engine takes effect when glance restarts."
    }
}
