//! Appearance: glance's accent colour or the macOS one.
#![allow(unsafe_code)]

use glance_orchestrator::settings::{
    SettingsBackend, SettingsChange, SettingsChoices, SettingsValues,
};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::sel;
use objc2_app_kit::{
    NSBox, NSBoxType, NSColor, NSSegmentSwitchTracking, NSSegmentedControl, NSTitlePosition, NSView,
};
use objc2_foundation::{NSArray, NSSize, NSString};

use super::ui::{self, Page, Ui};
use super::{Reply, Section};
use crate::theme;

const ACCENT: isize = 1;
const SOURCES: [(&str, &str); 2] = [("glance", "glance"), ("system", "System")];

pub struct Appearance {
    page: Page,
    picker: Retained<NSSegmentedControl>,
    swatch: Retained<NSBox>,
}

impl Appearance {
    pub fn new(ui: &Ui<'_>) -> Self {
        let mtm = ui.mtm;
        let page = Page::new(mtm);
        let labels: Vec<Retained<NSString>> =
            SOURCES.iter().map(|(_, l)| NSString::from_str(l)).collect();
        // SAFETY: the target is the window's (it outlives the control) and
        // `changed:` takes one object argument.
        let picker = unsafe {
            NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
                &NSArray::from_retained_slice(&labels),
                NSSegmentSwitchTracking::SelectOne,
                Some(ui.target as &AnyObject),
                Some(sel!(changed:)),
                mtm,
            )
        };
        picker.setTag(ui.page * 100 + ACCENT);
        ui::auto(&picker);
        let swatch = NSBox::new(mtm);
        swatch.setBoxType(NSBoxType::Custom);
        swatch.setTitlePosition(NSTitlePosition::NoTitle);
        swatch.setCornerRadius(6.0);
        swatch.setBorderWidth(1.0);
        swatch.setBorderColor(&NSColor::separatorColor());
        swatch.setContentViewMargins(NSSize::new(0.0, 0.0));
        ui::auto(&swatch);
        ui::activate(&[
            swatch.widthAnchor().constraintEqualToConstant(44.0),
            swatch.heightAnchor().constraintEqualToConstant(22.0),
        ]);
        let control: Retained<NSView> =
            Retained::into_super(ui::stack(mtm, &[&swatch, &picker], false, 10.0));
        page.group(
            mtm,
            "Colour",
            &[ui::row(
                mtm,
                "Accent colour",
                Some(
                    "glance: a warm coral. System: your macOS accent. Used for the overlay's \
                     state, the selected section, sliders and main buttons.",
                ),
                false,
                &control,
            )],
        );
        let this = Self {
            page,
            picker,
            swatch,
        };
        this.paint();
        this
    }

    fn paint(&self) {
        self.swatch.setFillColor(&theme::accent());
        self.picker
            .setSelectedSegmentBezelColor(Some(&theme::accent()));
    }
}

impl Section for Appearance {
    fn name(&self) -> &'static str {
        "Appearance"
    }

    fn symbol(&self) -> &'static str {
        "paintpalette"
    }

    fn view(&self) -> Retained<NSView> {
        Retained::into_super(self.page.root.clone())
    }

    fn fill(&mut self, values: &SettingsValues, _choices: &SettingsChoices) {
        let at = SOURCES
            .iter()
            .position(|(s, _)| *s == values.accent)
            .unwrap_or(0);
        self.picker.setSelectedSegment(at as isize);
        self.paint();
    }

    fn on_change(&mut self, id: isize, backend: &dyn SettingsBackend) -> Reply {
        if id != ACCENT {
            return Reply::Nothing;
        }
        let at = usize::try_from(self.picker.selectedSegment())
            .unwrap_or(0)
            .min(1);
        accent(SOURCES[at].0, backend)
    }

    fn restore(&mut self, d: &SettingsValues, backend: &dyn SettingsBackend) -> Option<Reply> {
        Some(accent(&d.accent, backend))
    }

    fn restore_note(&self) -> &'static str {
        "The accent goes back to glance's coral."
    }
}

/// Save the accent and repaint glance's windows with it.
fn accent(source: &str, backend: &dyn SettingsBackend) -> Reply {
    let result = backend.apply(SettingsChange::Accent(source.to_owned()));
    if result.is_ok() {
        theme::set_system_accent(source == "system");
    }
    Reply::Repaint(result)
}
