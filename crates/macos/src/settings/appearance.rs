//! Appearance: glance's accent colour or the macOS one, and the screenshot
//! indicator (Glow, Subtle or Off; live, with a preview when changed).
#![allow(unsafe_code)]

use glance_orchestrator::settings::{
    Applied, SettingsBackend, SettingsChange, SettingsChoices, SettingsValues,
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
use crate::{glow, theme};

const ACCENT: isize = 1;
const INDICATOR: isize = 2;
const SOURCES: [(&str, &str); 2] = [("glance", "glance"), ("system", "System")];
const INDICATORS: [(&str, &str); 3] = [("glow", "Glow"), ("subtle", "Subtle"), ("off", "Off")];

pub struct Appearance {
    page: Page,
    picker: Retained<NSSegmentedControl>,
    swatch: Retained<NSBox>,
    indicator: Retained<NSSegmentedControl>,
}

/// A segmented control that reports to the window as `id` on this page.
fn segments(ui: &Ui<'_>, labels: &[(&str, &str)], id: isize) -> Retained<NSSegmentedControl> {
    let labels: Vec<Retained<NSString>> =
        labels.iter().map(|(_, l)| NSString::from_str(l)).collect();
    // SAFETY: the target is the window's (it outlives the control) and
    // `changed:` takes one object argument.
    let control = unsafe {
        NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
            &NSArray::from_retained_slice(&labels),
            NSSegmentSwitchTracking::SelectOne,
            Some(ui.target as &AnyObject),
            Some(sel!(changed:)),
            ui.mtm,
        )
    };
    control.setTag(ui.page * 100 + id);
    ui::auto(&control);
    control
}

impl Appearance {
    pub fn new(ui: &Ui<'_>) -> Self {
        let mtm = ui.mtm;
        let page = Page::new(mtm);
        let picker = segments(ui, &SOURCES, ACCENT);
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
        let indicator = segments(ui, &INDICATORS, INDICATOR);
        page.group(
            mtm,
            "Screenshot",
            &[ui::row(
                mtm,
                "Screenshot indicator",
                Some(
                    "Shown around the edge of the display glance just captured. The overlay \
                     also says when a screenshot is new.",
                ),
                false,
                &indicator,
            )],
        );
        let this = Self {
            page,
            picker,
            swatch,
            indicator,
        };
        this.paint();
        this
    }

    fn paint(&self) {
        self.swatch.setFillColor(&theme::accent());
        for control in [&self.picker, &self.indicator] {
            control.setSelectedSegmentBezelColor(Some(&theme::accent()));
        }
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
        let at = INDICATORS
            .iter()
            .position(|(s, _)| *s == values.screenshot_indicator)
            .unwrap_or(0);
        self.indicator.setSelectedSegment(at as isize);
        self.paint();
    }

    fn on_change(&mut self, id: isize, backend: &dyn SettingsBackend) -> Reply {
        match id {
            ACCENT => {
                let at = usize::try_from(self.picker.selectedSegment())
                    .unwrap_or(0)
                    .min(SOURCES.len() - 1);
                accent(SOURCES[at].0, backend)
            }
            INDICATOR => {
                let at = usize::try_from(self.indicator.selectedSegment())
                    .unwrap_or(0)
                    .min(INDICATORS.len() - 1);
                let result = indicator(INDICATORS[at].0, backend);
                // Show what was picked, on the display this window is on.
                if result.is_ok() {
                    glow::flash(glow::key_display());
                }
                Reply::Applied(result)
            }
            _ => Reply::Nothing,
        }
    }

    fn restore(&mut self, d: &SettingsValues, backend: &dyn SettingsBackend) -> Option<Reply> {
        if let Err(e) = indicator(&d.screenshot_indicator, backend) {
            return Some(Reply::Applied(Err(e)));
        }
        Some(accent(&d.accent, backend))
    }

    fn restore_note(&self) -> &'static str {
        "The accent goes back to glance's coral and the screenshot indicator to Glow."
    }
}

/// Save the screenshot indicator and use it from the next screenshot.
fn indicator(name: &str, backend: &dyn SettingsBackend) -> Result<Applied, String> {
    let result = backend.apply(SettingsChange::ScreenshotIndicator(name.to_owned()));
    if result.is_ok() {
        glow::set_indicator(name);
    }
    result
}

/// Save the accent and repaint glance's windows with it.
fn accent(source: &str, backend: &dyn SettingsBackend) -> Reply {
    let result = backend.apply(SettingsChange::Accent(source.to_owned()));
    if result.is_ok() {
        theme::set_system_accent(source == "system");
    }
    Reply::Repaint(result)
}
