//! About: version, licences, third-party notices.

use glance_orchestrator::settings::{SettingsBackend, SettingsChoices, SettingsValues};
use objc2::rc::Retained;
use objc2_app_kit::{NSFont, NSStackView, NSTextField, NSTextView, NSView};
use objc2_foundation::{NSSize, NSString};

use super::ui::{self, Page, Ui};
use super::{Reply, Section};

/// THIRD_PARTY.md, compiled in.
const NOTICES: &str = include_str!("../../../../THIRD_PARTY.md");

const SHOW_NOTICES: isize = 1;

pub struct About {
    page: Page,
    models: Retained<NSStackView>,
    notices: Retained<NSView>,
}

impl About {
    pub fn new(ui: &Ui<'_>) -> Self {
        let mtm = ui.mtm;
        let page = Page::new(mtm);

        let version = ui::label(mtm, env!("CARGO_PKG_VERSION"));
        page.group(
            mtm,
            "glance",
            &[
                ui::row(
                    mtm,
                    "Version",
                    Some("Local only: no telemetry, no update checks."),
                    false,
                    &version,
                ),
                ui::row(
                    mtm,
                    "glance licence",
                    Some("MIT OR Apache-2.0, at your option."),
                    false,
                    &ui.button("Licences…", SHOW_NOTICES, false),
                ),
            ],
        );

        let models = ui::stack(mtm, &[], true, 4.0);
        let holder: Retained<NSView> = Retained::into_super(models.clone());
        page.group(mtm, "Model licences (models.toml)", &[holder]);

        let scroll = NSTextView::scrollableTextView(mtm);
        if let Some(text) = scroll
            .documentView()
            .and_then(|v| v.downcast::<NSTextView>().ok())
        {
            text.setString(&NSString::from_str(NOTICES));
            text.setEditable(false);
            text.setFont(Some(&NSFont::systemFontOfSize(11.0)));
            text.setTextContainerInset(NSSize::new(4.0, 6.0));
        }
        scroll.setDrawsBackground(false);
        ui::auto(&scroll);
        ui::activate(&[scroll.heightAnchor().constraintEqualToConstant(260.0)]);
        let notices: Retained<NSView> = Retained::into_super(scroll);
        page.group(mtm, "Third-party notices", std::slice::from_ref(&notices));

        Self {
            page,
            models,
            notices,
        }
    }
}

impl Section for About {
    fn name(&self) -> &'static str {
        "About"
    }

    fn symbol(&self) -> &'static str {
        "info.circle"
    }

    fn view(&self) -> Retained<NSView> {
        Retained::into_super(self.page.root.clone())
    }

    fn fill(&mut self, _values: &SettingsValues, _choices: &SettingsChoices) {
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            return;
        };
        for old in self.models.arrangedSubviews().iter() {
            self.models.removeArrangedSubview(&old);
            old.removeFromSuperview();
        }
        let rows = crate::shell::models_backend()
            .map(|m| m.rows())
            .unwrap_or_default();
        for row in rows {
            let line: Retained<NSTextField> = ui::secondary(
                mtm,
                &format!("{}: {}", row.id, row.license),
                ui::TEXT_W * 1.6,
            );
            self.models.addArrangedSubview(&line);
        }
    }

    fn on_change(&mut self, id: isize, _backend: &dyn SettingsBackend) -> Reply {
        if id == SHOW_NOTICES {
            self.notices.scrollRectToVisible(self.notices.bounds());
        }
        Reply::Nothing
    }

    /// Nothing to restore here.
    fn restore_note(&self) -> &'static str {
        ""
    }
}
