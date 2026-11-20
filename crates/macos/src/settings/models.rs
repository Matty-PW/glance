//! Models: which models glance uses ("Apply and restart"), and every model
//! pinned in models.toml with download, check (re-hash), repair and delete.
//! The work is the app's `ModelsBackend`, the same pinned downloader as
//! `glance models pull`; downloads and deletes ask first. Progress is
//! polled twice a second while the page is shown.

use glance_orchestrator::settings::{
    Applied, ModelRow, ModelState, SettingsBackend, SettingsChange, SettingsChoices, SettingsValues,
};
use objc2::rc::Retained;
use objc2::{MainThreadMarker, Message};
use objc2_app_kit::{
    NSButton, NSColor, NSControlStateValueOn, NSFont, NSImageView, NSLayoutConstraintOrientation,
    NSPopUpButton, NSStackView, NSSwitch, NSTextField, NSView,
};
use objc2_foundation::NSString;

use super::ui::{self, Page, Ui};
use super::{Reply, Section};

const BUDGETS: [Option<u32>; 4] = [Some(64), Some(128), Some(256), None];

const FAST: isize = 1;
const DEEP: isize = 2;
const BUDGET: isize = 3;
const AUTO: isize = 4;
const APPLY: isize = 5;
/// Model row buttons: FIRST_ROW + row × 3 + action.
const FIRST_ROW: isize = 10;
const PRIMARY: isize = 0;
const CHECK: isize = 1;
const DELETE: isize = 2;

struct RowUi {
    title: Retained<NSTextField>,
    detail: Retained<NSTextField>,
    icon: Retained<NSImageView>,
    status: Retained<NSTextField>,
    primary: Retained<NSButton>,
    check: Retained<NSButton>,
    delete: Retained<NSButton>,
}

pub struct Models {
    page: Page,
    mtm: MainThreadMarker,
    target: Retained<super::ui::Target>,
    page_index: isize,
    fast: Retained<NSPopUpButton>,
    deep: Retained<NSPopUpButton>,
    budget: Retained<NSPopUpButton>,
    auto: Retained<NSSwitch>,
    fast_items: Vec<String>,
    deep_items: Vec<Option<String>>,
    list: Retained<NSStackView>,
    free: Retained<NSTextField>,
    rows: Vec<RowUi>,
    data: Vec<ModelRow>,
}

impl Models {
    pub fn new(ui: &Ui<'_>) -> Self {
        let mtm = ui.mtm;
        let page = Page::new(mtm);
        let fast = ui.popup(&[], FAST);
        let deep = ui.popup(&[], DEEP);
        let budget = ui.popup(
            &BUDGETS.map(|b| b.map_or("No cap".to_owned(), |n| format!("{n} tokens"))),
            BUDGET,
        );
        let auto = ui.switch(AUTO);
        let apply = ui.button("Apply and restart", APPLY, true);
        page.group(
            mtm,
            "In use",
            &[
                ui::row(
                    mtm,
                    "FAST model",
                    Some("Answers every question."),
                    true,
                    &fast,
                ),
                ui::row(
                    mtm,
                    "DEEP model",
                    Some("Loaded only for think harder."),
                    true,
                    &deep,
                ),
                ui::row(
                    mtm,
                    "DEEP thinking cap",
                    Some("Stops DEEP's thinking after this many tokens."),
                    true,
                    &budget,
                ),
                ui::row(
                    mtm,
                    "Let FAST hand hard questions to DEEP",
                    Some("Keeps DEEP loaded all the time."),
                    true,
                    &auto,
                ),
                ui::row(mtm, "", None, false, &apply),
            ],
        );
        let list = ui::stack(mtm, &[], true, 10.0);
        let free = ui::secondary(mtm, "", ui::TEXT_W * 2.0);
        let holder: Retained<NSView> = Retained::into_super(list.clone());
        page.group(mtm, "Pinned models", &[holder]);
        page.add(&free);
        Self {
            page,
            mtm,
            target: ui.target.retain(),
            page_index: ui.page,
            fast,
            deep,
            budget,
            auto,
            fast_items: Vec::new(),
            deep_items: Vec::new(),
            list,
            free,
            rows: Vec::new(),
            data: Vec::new(),
        }
    }

    fn ui(&self) -> Ui<'_> {
        Ui {
            mtm: self.mtm,
            target: &self.target,
            page: self.page_index,
        }
    }

    /// (Re)build one row per model.
    fn build_rows(&mut self, count: usize) {
        for old in self.list.arrangedSubviews().iter() {
            self.list.removeArrangedSubview(&old);
            old.removeFromSuperview();
        }
        self.rows.clear();
        let mtm = self.mtm;
        for i in 0..count {
            if i > 0 {
                let line = objc2_app_kit::NSBox::new(mtm);
                line.setBoxType(objc2_app_kit::NSBoxType::Separator);
                self.list.addArrangedSubview(&line);
                ui::full_width(&line, &self.list, 0.0);
            }
            let id = |action: isize| FIRST_ROW + i as isize * 3 + action;
            let ui = self.ui();
            let primary = ui.button("Download", id(PRIMARY), false);
            let check = ui.button("Check", id(CHECK), false);
            let delete = ui.button("Delete", id(DELETE), false);
            let title = ui::label(mtm, "");
            title.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
            let detail = ui::secondary(mtm, "", ui::TEXT_W);
            let icon = NSImageView::new(mtm);
            icon.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
            let status = ui::label(mtm, "");
            status.setFont(Some(&NSFont::systemFontOfSize(12.0)));
            let status_row = ui::stack(mtm, &[&icon, &status], false, 4.0);
            let text = ui::stack(mtm, &[&title, &detail, &status_row], true, 2.0);
            let buttons = ui::stack(mtm, &[&primary, &check, &delete], false, 6.0);
            // Exactly as wide as the buttons shown, so they sit on the right.
            buttons.setHuggingPriority_forOrientation(
                751.0,
                NSLayoutConstraintOrientation::Horizontal,
            );
            let row = ui::row_with(mtm, &text, &buttons);
            self.list.addArrangedSubview(&row);
            ui::full_width(&row, &self.list, 0.0);
            self.rows.push(RowUi {
                title,
                detail,
                icon,
                status,
                primary,
                check,
                delete,
            });
        }
    }

    fn show_rows(&mut self) {
        let Some(backend) = crate::shell::models_backend() else {
            return;
        };
        let data = backend.rows();
        if data.len() != self.rows.len() {
            self.build_rows(data.len());
        }
        let any_busy = data.iter().any(|m| m.state.is_busy());
        for (row, model) in self.rows.iter().zip(&data) {
            row.title.setStringValue(&NSString::from_str(&model.id));
            // Long ids truncate; the full one is in the tooltip.
            row.title.setToolTip(Some(&NSString::from_str(&model.id)));
            row.detail
                .setStringValue(&NSString::from_str(&detail_line(model)));
            row.status
                .setStringValue(&NSString::from_str(&status_line(&model.state)));
            row.icon.setImage(
                super::symbol(
                    status_symbol(&model.state),
                    status_line(&model.state).as_str(),
                )
                .as_deref(),
            );
            let complete = model.bytes_missing == 0;
            let busy = model.state.is_busy();
            let failed = matches!(model.state, ModelState::Failed(_));
            row.primary.setTitle(&NSString::from_str(if busy {
                "Cancel"
            } else if failed && complete {
                "Repair"
            } else {
                "Download"
            }));
            row.primary.setHidden(complete && !busy && !failed);
            row.primary.setEnabled(busy || !any_busy);
            row.check.setHidden(!complete);
            row.check.setEnabled(!any_busy);
            row.delete.setHidden(model.bytes_missing == model.bytes);
            row.delete.setEnabled(!busy && model.in_use.is_none());
            let tip = model
                .in_use
                .as_ref()
                .map(|used| format!("In use ({used}); choose another model first."));
            row.delete
                .setToolTip(tip.as_deref().map(NSString::from_str).as_deref());
        }
        let free = backend
            .free_bytes()
            .map_or("Free space: unknown".to_owned(), |b| {
                format!("Free space: {}", gb(b))
            });
        self.free.setStringValue(&NSString::from_str(&format!(
            "{free}. Downloads come only from Hugging Face at the pinned commit and must match each file's SHA-256."
        )));
        self.data = data;
    }

    fn on_model(&mut self, i: usize, action: isize) -> Reply {
        let (Some(backend), Some(model), Some(mtm)) = (
            crate::shell::models_backend(),
            self.data.get(i).cloned(),
            MainThreadMarker::new(),
        ) else {
            return Reply::Nothing;
        };
        let result: Result<Option<String>, String> = match action {
            PRIMARY if model.state.is_busy() => {
                backend.cancel();
                Ok(Some(format!("Stopped {}.", model.id)))
            }
            PRIMARY => {
                let free = backend.free_bytes().map_or("unknown".to_owned(), gb);
                let revision: String = model.revision.chars().take(12).collect();
                let text = format!(
                    "{} to download from huggingface.co/{} at commit {revision} (licence: {}). \
                     Each file is checked against its pinned SHA-256 before glance uses it.\n\n\
                     Free space: {free}.",
                    gb(model.bytes_missing),
                    model.repo,
                    model.license
                );
                if super::confirm(mtm, &format!("Download {}?", model.id), &text, "Download") {
                    backend.download(&model.id).map(|()| None)
                } else {
                    Ok(None)
                }
            }
            CHECK => backend.verify(&model.id).map(|()| None),
            DELETE => {
                let text = format!(
                    "This frees {} on disk. You can download it again later.",
                    gb(model.bytes - model.bytes_missing)
                );
                if super::confirm(mtm, &format!("Delete {}?", model.id), &text, "Delete") {
                    backend
                        .delete(&model.id)
                        .map(|freed| Some(format!("Deleted {} ({}).", model.id, gb(freed))))
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None),
        };
        self.show_rows();
        match result {
            Ok(Some(text)) => Reply::Say(text),
            Ok(None) => Reply::Nothing,
            Err(e) => Reply::Say(e),
        }
    }

    fn models_change(&self) -> SettingsChange {
        SettingsChange::Models {
            fast: self.fast_items[ui::index(&self.fast, self.fast_items.len())].clone(),
            deep: self.deep_items[ui::index(&self.deep, self.deep_items.len())].clone(),
            auto_escalate: self.auto.state() == NSControlStateValueOn,
            thinking_budget: BUDGETS[ui::index(&self.budget, BUDGETS.len())],
        }
    }
}

impl Section for Models {
    fn name(&self) -> &'static str {
        "Models"
    }

    fn symbol(&self) -> &'static str {
        "cpu"
    }

    fn view(&self) -> Retained<NSView> {
        Retained::into_super(self.page.root.clone())
    }

    fn fill(&mut self, values: &SettingsValues, choices: &SettingsChoices) {
        self.fast_items = choices.models.clone();
        if !self.fast_items.contains(&values.fast_model) {
            self.fast_items.insert(0, values.fast_model.clone());
        }
        ui::set_items(&self.fast, &self.fast_items);
        ui::select(&self.fast, &self.fast_items, &values.fast_model);
        self.deep_items = std::iter::once(None)
            .chain(choices.models.iter().cloned().map(Some))
            .collect();
        if !self.deep_items.contains(&values.deep_model) {
            self.deep_items.insert(1, values.deep_model.clone());
        }
        let titles: Vec<String> = self
            .deep_items
            .iter()
            .map(|d| {
                d.clone()
                    .unwrap_or_else(|| "None (no think harder)".to_owned())
            })
            .collect();
        ui::set_items(&self.deep, &titles);
        let deep = self
            .deep_items
            .iter()
            .position(|d| *d == values.deep_model)
            .unwrap_or(0);
        self.deep.selectItemAtIndex(deep as isize);
        let budget = BUDGETS
            .iter()
            .position(|b| *b == values.deep_thinking_budget)
            .unwrap_or(1);
        self.budget.selectItemAtIndex(budget as isize);
        self.auto.setState(ui::on_off(values.auto_escalate));
        self.show_rows();
    }

    fn on_change(&mut self, id: isize, backend: &dyn SettingsBackend) -> Reply {
        match id {
            APPLY => match backend.apply(self.models_change()) {
                Ok(Applied::NeedsRestart) => Reply::Restart,
                other => Reply::Applied(other),
            },
            // Choices wait for "Apply and restart".
            FAST | DEEP | BUDGET | AUTO => {
                Reply::Say("Press Apply and restart to use the new choice.".into())
            }
            id if id >= FIRST_ROW => {
                let n = id - FIRST_ROW;
                self.on_model((n / 3) as usize, n % 3)
            }
            _ => Reply::Nothing,
        }
    }

    fn restore(&mut self, d: &SettingsValues, backend: &dyn SettingsBackend) -> Option<Reply> {
        Some(
            match backend.apply(SettingsChange::Models {
                fast: d.fast_model.clone(),
                deep: d.deep_model.clone(),
                auto_escalate: d.auto_escalate,
                thinking_budget: d.deep_thinking_budget,
            }) {
                Ok(Applied::NeedsRestart) => Reply::Restart,
                other => Reply::Applied(other),
            },
        )
    }

    fn restore_note(&self) -> &'static str {
        "FAST goes back to the default model, DEEP to none, the thinking cap to 128 tokens and hand-over off. glance restarts to apply. Downloaded models are kept."
    }

    fn tick(&mut self) {
        self.show_rows();
    }
}

fn detail_line(model: &ModelRow) -> String {
    let mut parts = vec![
        model.role.clone(),
        gb(model.bytes),
        model.license.clone(),
        model.repo.clone(),
    ];
    if let Some(used) = &model.in_use {
        parts.push(format!("in use: {used}"));
    }
    parts.join(" · ")
}

fn status_line(state: &ModelState) -> String {
    match state {
        ModelState::NotDownloaded => "Not downloaded".into(),
        ModelState::Partial { present, total } => {
            format!("Incomplete: {present} of {total} files")
        }
        ModelState::Downloaded => "Downloaded".into(),
        ModelState::Verified => "Checked against its pins".into(),
        ModelState::Failed(e) => format!("Failed: {e}"),
        ModelState::Preparing => "Checking files…".into(),
        ModelState::Downloading { done, total } => format!(
            "Downloading {}% ({} of {})",
            done * 100 / (*total).max(1),
            gb(*done),
            gb(*total)
        ),
        ModelState::Verifying { file, files } => format!("Checking file {file} of {files}…"),
    }
}

/// A symbol per state, so the state never depends on colour.
fn status_symbol(state: &ModelState) -> &'static str {
    match state {
        ModelState::NotDownloaded => "arrow.down.circle",
        ModelState::Partial { .. } => "circle.lefthalf.filled",
        ModelState::Downloaded => "checkmark.circle",
        ModelState::Verified => "checkmark.seal",
        ModelState::Failed(_) => "exclamationmark.triangle",
        ModelState::Preparing | ModelState::Verifying { .. } => "magnifyingglass",
        ModelState::Downloading { .. } => "arrow.down.circle.dotted",
    }
}

fn gb(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1e9)
    } else {
        format!("{:.0} MB", (bytes as f64 / 1e6).max(1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_progress_read_naturally() {
        assert_eq!(gb(17_400_000_000), "17.4 GB");
        assert_eq!(gb(330_000_000), "330 MB");
        assert_eq!(gb(10), "1 MB");
        assert_eq!(
            status_line(&ModelState::Downloading {
                done: 1_100_000_000,
                total: 2_700_000_000
            }),
            "Downloading 40% (1.1 GB of 2.7 GB)"
        );
    }
}
