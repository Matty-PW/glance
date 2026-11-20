//! Think harder: DEEP's reasoning effort and when it is unloaded.

use glance_orchestrator::settings::{
    SettingsBackend, SettingsChange, SettingsChoices, SettingsValues,
};
use objc2::rc::Retained;
use objc2_app_kit::{NSPopUpButton, NSView};

use super::ui::{self, Page, Ui};
use super::{Reply, Section};

const EFFORTS: [(&str, &str); 4] = [
    ("off", "Off (no thinking)"),
    ("low", "Low"),
    ("medium", "Medium"),
    ("xhigh", "Extra high"),
];
const IDLE_MINS: [u64; 5] = [5, 10, 20, 30, 60];

const EFFORT: isize = 1;
const IDLE: isize = 2;

pub struct Think {
    page: Page,
    effort: Retained<NSPopUpButton>,
    idle: Retained<NSPopUpButton>,
}

impl Think {
    pub fn new(ui: &Ui<'_>) -> Self {
        let mtm = ui.mtm;
        let page = Page::new(mtm);
        let effort = ui.popup(&EFFORTS.map(|(_, l)| l.to_owned()), EFFORT);
        let idle = ui.popup(&IDLE_MINS.map(|m| format!("After {m} minutes idle")), IDLE);
        page.group(
            mtm,
            "DEEP (the bigger model)",
            &[
                ui::row(
                    mtm,
                    "Reasoning effort",
                    Some("More effort is slower to the first word."),
                    false,
                    &effort,
                ),
                ui::row(
                    mtm,
                    "Unload DEEP",
                    Some("Frees its memory when think harder has not been used for a while."),
                    false,
                    &idle,
                ),
            ],
        );
        Self { page, effort, idle }
    }

    fn change(&self) -> SettingsChange {
        SettingsChange::Think {
            effort: EFFORTS[ui::index(&self.effort, EFFORTS.len())].0.to_owned(),
            idle_mins: IDLE_MINS[ui::index(&self.idle, IDLE_MINS.len())],
        }
    }
}

impl Section for Think {
    fn name(&self) -> &'static str {
        "Think harder"
    }

    fn symbol(&self) -> &'static str {
        "brain"
    }

    fn view(&self) -> Retained<NSView> {
        Retained::into_super(self.page.root.clone())
    }

    fn fill(&mut self, values: &SettingsValues, _choices: &SettingsChoices) {
        let effort = EFFORTS
            .iter()
            .position(|(e, _)| *e == values.deep_effort)
            .unwrap_or(2);
        self.effort.selectItemAtIndex(effort as isize);
        let idle = IDLE_MINS
            .iter()
            .position(|m| *m == values.deep_idle_mins)
            .unwrap_or(1);
        self.idle.selectItemAtIndex(idle as isize);
    }

    fn on_change(&mut self, id: isize, backend: &dyn SettingsBackend) -> Reply {
        match id {
            EFFORT | IDLE => Reply::Applied(backend.apply(self.change())),
            _ => Reply::Nothing,
        }
    }

    fn restore(&mut self, d: &SettingsValues, backend: &dyn SettingsBackend) -> Option<Reply> {
        Some(Reply::Applied(backend.apply(SettingsChange::Think {
            effort: d.deep_effort.clone(),
            idle_mins: d.deep_idle_mins,
        })))
    }

    fn restore_note(&self) -> &'static str {
        "Reasoning effort goes back to medium and DEEP unloads after 10 minutes idle."
    }
}
