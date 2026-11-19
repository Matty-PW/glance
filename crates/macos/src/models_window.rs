//! The models window: every model pinned in `models.toml`, with its size,
//! licence and source, whether it is downloaded and checked, and buttons to
//! download, check (re-hash) and delete it. The work is the app's
//! [`ModelsBackend`](glance_orchestrator::settings::ModelsBackend), which uses
//! the same pinned downloader as `glance models pull`.
//!
//! Downloads and deletes ask first (size, source, free space). The window
//! polls the backend twice a second while it is open.
//!
//! Main thread only. `unsafe` here: AppKit's target/action wiring (controls
//! keep their target weakly, so the window owns it) and
//! `setReleasedWhenClosed`.
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::sync::Once;
use std::time::Duration;

use dispatch2::DispatchQueue;
use glance_orchestrator::settings::{ModelRow, ModelState};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSApplication, NSBackingStoreType, NSButton,
    NSColor, NSControl, NSFont, NSLineBreakMode, NSPanel, NSScrollView, NSTextField, NSView,
    NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

const WIDTH: f64 = 760.0;
const ROW_H: f64 = 62.0;
const TOP: f64 = 44.0;
const BOTTOM: f64 = 52.0;
const MAX_LIST_H: f64 = 640.0;
const PAD: f64 = 20.0;
const BUTTON_W: f64 = 92.0;

/// Button actions; a button's tag is `row * 3 + action + 1`.
const PRIMARY: isize = 0;
const VERIFY: isize = 1;
const DELETE: isize = 2;

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `Target` has no state and does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "GlanceModelsTarget"]
    struct Target;

    unsafe impl NSObjectProtocol for Target {}

    impl Target {
        #[unsafe(method(clicked:))]
        fn clicked(&self, sender: &NSControl) {
            let tag = sender.tag() - 1;
            // Not inside the action: alerts and the backend run after it.
            DispatchQueue::main().exec_async(move || on_click((tag / 3) as usize, tag % 3));
        }
    }
);

impl Target {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: plain `-[NSObject init]` on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }
}

struct Row {
    title: Retained<NSTextField>,
    detail: Retained<NSTextField>,
    status: Retained<NSTextField>,
    primary: Retained<NSButton>,
    verify: Retained<NSButton>,
    delete: Retained<NSButton>,
}

struct Window {
    panel: Retained<NSPanel>,
    /// Controls hold their target weakly; the window keeps it alive.
    _target: Retained<Target>,
    rows: Vec<Row>,
    /// What the rows showed at the last refresh, in the same order.
    data: Vec<ModelRow>,
    free: Retained<NSTextField>,
    message: Retained<NSTextField>,
}

thread_local! {
    static WINDOW: RefCell<Option<Window>> = const { RefCell::new(None) };
}

/// Show the window (tray menu, or the settings window). Main thread.
pub fn open() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Some(backend) = crate::shell::models_backend() else {
        return;
    };
    let data = backend.rows();
    let opened = WINDOW.try_with(|cell| {
        let Ok(mut window) = cell.try_borrow_mut() else {
            return;
        };
        if window.as_ref().is_none_or(|w| w.rows.len() != data.len()) {
            *window = Some(Window::build(mtm, data.len()));
        }
        let Some(w) = window.as_mut() else { return };
        w.message.setStringValue(&NSString::from_str(""));
        w.show(data, backend.free_bytes());
        w.panel.center();
        w.panel.makeKeyAndOrderFront(None);
    });
    if opened.is_ok() {
        NSApplication::sharedApplication(mtm).activate();
        start_polling();
    }
}

/// Refresh twice a second while the window is visible.
fn start_polling() {
    static STARTED: Once = Once::new();
    STARTED.call_once(|| {
        std::thread::spawn(|| {
            loop {
                std::thread::sleep(Duration::from_millis(500));
                DispatchQueue::main().exec_async(tick);
            }
        });
    });
}

fn tick() {
    let Some(backend) = crate::shell::models_backend() else {
        return;
    };
    let _ = WINDOW.try_with(|cell| {
        // Busy (an alert is up and this ran inside its loop): skip a beat.
        let Ok(mut window) = cell.try_borrow_mut() else {
            return;
        };
        let Some(w) = window.as_mut() else { return };
        if !w.panel.isVisible() {
            return;
        }
        let data = backend.rows();
        if data.len() == w.rows.len() {
            w.show(data, backend.free_bytes());
        }
    });
}

fn on_click(row: usize, action: isize) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Some(backend) = crate::shell::models_backend() else {
        return;
    };
    // Read what the row shows, then let go of the window before any alert.
    let Some(model) = WINDOW.with_borrow(|w| w.as_ref().and_then(|w| w.data.get(row).cloned()))
    else {
        return;
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
            if confirm(
                mtm,
                &format!("Download {}?", model.id),
                &text,
                "Download",
                false,
            ) {
                backend.download(&model.id).map(|()| None)
            } else {
                Ok(None)
            }
        }
        VERIFY => backend.verify(&model.id).map(|()| None),
        DELETE => {
            let text = format!(
                "This frees {} on disk. You can download it again later.",
                gb(model.bytes - model.bytes_missing)
            );
            if confirm(mtm, &format!("Delete {}?", model.id), &text, "Delete", true) {
                backend
                    .delete(&model.id)
                    .map(|freed| Some(format!("Deleted {} ({}).", model.id, gb(freed))))
            } else {
                Ok(None)
            }
        }
        _ => Ok(None),
    };
    WINDOW.with_borrow_mut(|window| {
        let Some(w) = window.as_mut() else { return };
        match &result {
            Ok(Some(text)) => w.message.setStringValue(&NSString::from_str(text)),
            Ok(None) => w.message.setStringValue(&NSString::from_str("")),
            Err(e) => w.message.setStringValue(&NSString::from_str(e)),
        }
        let data = backend.rows();
        if data.len() == w.rows.len() {
            w.show(data, backend.free_bytes());
        }
    });
}

/// A modal yes/no question; true for `yes`.
fn confirm(mtm: MainThreadMarker, title: &str, text: &str, yes: &str, warning: bool) -> bool {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(text));
    alert.addButtonWithTitle(&NSString::from_str(yes));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    if warning {
        alert.setAlertStyle(NSAlertStyle::Critical);
    }
    alert.runModal() == NSAlertFirstButtonReturn
}

impl Window {
    fn build(mtm: MainThreadMarker, count: usize) -> Self {
        let list_h = (count as f64 * ROW_H).min(MAX_LIST_H);
        let height = TOP + list_h + BOTTOM;
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, height));
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            frame,
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
            NSBackingStoreType::Buffered,
            false,
        );
        panel.setTitle(&NSString::from_str("glance models"));
        panel.setHidesOnDeactivate(false);
        // SAFETY: the window is owned by `WINDOW` for the app's lifetime; it
        // must not free itself when closed.
        unsafe { panel.setReleasedWhenClosed(false) };
        let view = NSView::initWithFrame(NSView::alloc(mtm), frame);
        panel.setContentView(Some(&view));
        let target = Target::new(mtm);

        let intro = small(
            mtm,
            "Models pinned in models.toml. Downloads come only from Hugging Face at the pinned \
             commit and must match each file's SHA-256.",
        );
        intro.setFrame(NSRect::new(
            NSPoint::new(PAD, height - TOP + 6.0),
            NSSize::new(WIDTH - 2.0 * PAD, 30.0),
        ));
        view.addSubview(&intro);

        let doc_h = count as f64 * ROW_H;
        let scroll = NSScrollView::initWithFrame(
            NSScrollView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, BOTTOM), NSSize::new(WIDTH, list_h)),
        );
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        scroll.setDrawsBackground(false);
        let doc = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH - 16.0, doc_h)),
        );
        scroll.setDocumentView(Some(&doc));
        view.addSubview(&scroll);

        let text_w = WIDTH - 2.0 * PAD - 3.0 * (BUTTON_W + 6.0) - 16.0;
        let rows = (0..count)
            .map(|i| {
                // Row 0 at the top of the (unflipped) document view.
                let y = doc_h - (i as f64 + 1.0) * ROW_H;
                let title = NSTextField::labelWithString(&NSString::from_str(""), mtm);
                title.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
                title.setFrame(NSRect::new(
                    NSPoint::new(PAD, y + 38.0),
                    NSSize::new(text_w, 18.0),
                ));
                let detail = small(mtm, "");
                detail.setFrame(NSRect::new(
                    NSPoint::new(PAD, y + 22.0),
                    NSSize::new(text_w, 16.0),
                ));
                let status = NSTextField::labelWithString(&NSString::from_str(""), mtm);
                status.setFont(Some(&NSFont::systemFontOfSize(12.0)));
                status.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
                status.setFrame(NSRect::new(
                    NSPoint::new(PAD, y + 5.0),
                    NSSize::new(text_w, 16.0),
                ));
                let button = |action: isize, title: &str, slot: f64| {
                    let b = button(mtm, title, &target, i as isize * 3 + action + 1);
                    b.setFrame(NSRect::new(
                        NSPoint::new(WIDTH - 16.0 - PAD - slot * (BUTTON_W + 6.0), y + 18.0),
                        NSSize::new(BUTTON_W, 28.0),
                    ));
                    doc.addSubview(&b);
                    b
                };
                let primary = button(PRIMARY, "Download", 3.0);
                let verify = button(VERIFY, "Check", 2.0);
                let delete = button(DELETE, "Delete", 1.0);
                for label in [&title, &detail, &status] {
                    doc.addSubview(label);
                }
                Row {
                    title,
                    detail,
                    status,
                    primary,
                    verify,
                    delete,
                }
            })
            .collect();
        // Start at the top of the list.
        doc.scrollPoint(NSPoint::new(0.0, (doc_h - list_h).max(0.0)));

        let free = small(mtm, "");
        free.setFrame(NSRect::new(
            NSPoint::new(PAD, 28.0),
            NSSize::new(WIDTH - 2.0 * PAD, 16.0),
        ));
        let message = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        message.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        message.setFrame(NSRect::new(
            NSPoint::new(PAD, 8.0),
            NSSize::new(WIDTH - 2.0 * PAD, 18.0),
        ));
        view.addSubview(&free);
        view.addSubview(&message);

        Self {
            panel,
            _target: target,
            rows,
            data: Vec::new(),
            free,
            message,
        }
    }

    fn show(&mut self, data: Vec<ModelRow>, free: Option<u64>) {
        let any_busy = data.iter().any(|m| m.state.is_busy());
        for (row, model) in self.rows.iter().zip(&data) {
            row.title.setStringValue(&NSString::from_str(&model.id));
            row.detail
                .setStringValue(&NSString::from_str(&detail_line(model)));
            row.status
                .setStringValue(&NSString::from_str(&status_line(&model.state)));
            let color = match model.state {
                ModelState::Failed(_) => NSColor::systemRedColor(),
                ModelState::Verified => NSColor::systemGreenColor(),
                _ => NSColor::labelColor(),
            };
            row.status.setTextColor(Some(&color));
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
            row.verify.setHidden(!complete);
            row.verify.setEnabled(!any_busy);
            row.delete.setHidden(model.bytes_missing == model.bytes);
            row.delete.setEnabled(!busy && model.in_use.is_none());
            if let Some(used) = &model.in_use {
                let tip = format!("In use ({used}); choose another model in Settings first.");
                row.delete.setToolTip(Some(&NSString::from_str(&tip)));
            } else {
                row.delete.setToolTip(None);
            }
        }
        let free = free.map_or("Free space: unknown".to_owned(), |b| {
            format!("Free space: {}", gb(b))
        });
        self.free.setStringValue(&NSString::from_str(&free));
        self.data = data;
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
        ModelState::Downloaded => "Downloaded (checked by hash whenever it is loaded)".into(),
        ModelState::Verified => "Downloaded and checked: every file matches its pin".into(),
        ModelState::Failed(e) => format!("Failed: {e}"),
        ModelState::Preparing => "Checking the files already here…".into(),
        ModelState::Downloading { done, total } => format!(
            "Downloading {}% ({} of {})",
            done * 100 / (*total).max(1),
            gb(*done),
            gb(*total)
        ),
        ModelState::Verifying { file, files } => format!("Checking file {file} of {files}…"),
    }
}

fn gb(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1e9)
    } else {
        format!("{:.0} MB", (bytes as f64 / 1e6).max(1.0))
    }
}

fn small(mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
    let l = NSTextField::wrappingLabelWithString(&NSString::from_str(text), mtm);
    l.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    l.setTextColor(Some(&NSColor::secondaryLabelColor()));
    l
}

fn button(mtm: MainThreadMarker, title: &str, target: &Target, tag: isize) -> Retained<NSButton> {
    // SAFETY: no target yet; it is set below.
    let b = unsafe {
        NSButton::buttonWithTitle_target_action(&NSString::from_str(title), None, None, mtm)
    };
    b.setTag(tag);
    let target: &AnyObject = target;
    // SAFETY: `target` is owned by the window, which also owns the button,
    // so it outlives it; `clicked:` takes one object argument.
    unsafe {
        b.setTarget(Some(target));
        b.setAction(Some(sel!(clicked:)));
    }
    b
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
