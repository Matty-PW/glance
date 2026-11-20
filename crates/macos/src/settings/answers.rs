//! Answers: how long spoken answers are.

use glance_orchestrator::ResponseLength;
use glance_orchestrator::settings::{
    SettingsBackend, SettingsChange, SettingsChoices, SettingsValues,
};
use objc2::rc::Retained;
use objc2_app_kit::{NSSlider, NSTextField, NSView};
use objc2_foundation::NSString;

use super::ui::{self, Page, Ui};
use super::{Reply, Section};

const LENGTHS: [ResponseLength; 3] = [
    ResponseLength::Short,
    ResponseLength::Medium,
    ResponseLength::Long,
];

const LENGTH: isize = 1;

pub struct Answers {
    page: Page,
    length: Retained<NSSlider>,
    length_label: Retained<NSTextField>,
}

impl Answers {
    pub fn new(ui: &Ui<'_>) -> Self {
        let mtm = ui.mtm;
        let page = Page::new(mtm);
        let length = ui.slider(0.0, 2.0, Some(3), LENGTH);
        let length_label = ui.value_label(56.0);
        let control: Retained<NSView> =
            Retained::into_super(ui::stack(mtm, &[&length, &length_label], false, 8.0));
        ui::activate(&[control.widthAnchor().constraintEqualToConstant(ui::CONTROL_W)]);
        page.group(
            mtm,
            "Length",
            &[ui::row(
                mtm,
                "Answer length",
                Some(
                    "Short: about two sentences. Medium: as the model answers. Long: more detail. \
                     \u{201c}Walk me through\u{201d} or \u{201c}in detail\u{201d} always gets the full answer.",
                ),
                false,
                &control,
            )],
        );
        Self {
            page,
            length,
            length_label,
        }
    }

    fn length(&self) -> ResponseLength {
        LENGTHS[(self.length.doubleValue().round() as usize).min(2)]
    }

    fn show_length(&self) {
        let name = match self.length() {
            ResponseLength::Short => "Short",
            ResponseLength::Medium => "Medium",
            ResponseLength::Long => "Long",
        };
        self.length_label.setStringValue(&NSString::from_str(name));
    }
}

impl Section for Answers {
    fn name(&self) -> &'static str {
        "Answers"
    }

    fn symbol(&self) -> &'static str {
        "text.bubble"
    }

    fn view(&self) -> Retained<NSView> {
        Retained::into_super(self.page.root.clone())
    }

    fn fill(&mut self, values: &SettingsValues, _choices: &SettingsChoices) {
        let at = LENGTHS.iter().position(|l| *l == values.length).unwrap_or(1);
        self.length.setDoubleValue(at as f64);
        self.show_length();
    }

    fn on_change(&mut self, id: isize, backend: &dyn SettingsBackend) -> Reply {
        if id != LENGTH {
            return Reply::Nothing;
        }
        self.show_length();
        Reply::Applied(backend.apply(SettingsChange::Length(self.length())))
    }

    fn restore(&mut self, d: &SettingsValues, backend: &dyn SettingsBackend) -> Option<Reply> {
        Some(Reply::Applied(backend.apply(SettingsChange::Length(d.length))))
    }

    fn restore_note(&self) -> &'static str {
        "Answer length goes back to medium."
    }
}
