//! The settings window: a sidebar of sections (SF Symbols, sentence-case
//! names) and a page per section with a title, rounded groups of rows, a
//! "Restore defaults…" button and a status line. Auto Layout throughout;
//! a fixed minimum size, and everything else resizes.
//!
//! The window only shows and edits plain values. Reading and saving them is
//! the app's [`SettingsBackend`]; models are its [`ModelsBackend`]
//! (glance_orchestrator::settings). How changes apply is unchanged
//! (maintainer, 2026-11-18): voice, length, personality, maths, think-harder
//! options and accent live; hotkeys live only if every new chord registers
//! (otherwise the old ones stay); models via "Apply and restart". Rows that
//! need a restart say so.
//!
//! Main thread only.
#![allow(unsafe_code)]

mod about;
mod answers;
mod appearance;
mod hotkeys;
mod models;
mod personality;
mod think;
mod ui;
mod voice;

use std::cell::RefCell;

use dispatch2::DispatchQueue;
use glance_orchestrator::settings::{Applied, SettingsBackend, SettingsChoices, SettingsValues};
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSApplication, NSBackingStoreType, NSBox,
    NSBoxType, NSButton, NSCellImagePosition, NSColor, NSFont, NSImage,
    NSLayoutConstraintOrientation, NSScrollView, NSStackView, NSTextField, NSTitlePosition, NSView,
    NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectView, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use crate::theme;
use ui::{Target, Ui, activate, auto, pin, stack};

const SIDEBAR_W: f64 = 200.0;
const MIN_W: f64 = 760.0;
const MIN_H: f64 = 540.0;
/// Sidebar buttons' tags start here (control tags are page × 100 + id).
const SIDEBAR_TAG: isize = 9_000;
const RESTORE_TAG: isize = 9_900;

/// What a page asks the window to do after a change.
pub(crate) enum Reply {
    Nothing,
    /// Saved (or not); the status line says which.
    Applied(Result<Applied, String>),
    Say(String),
    /// Saved; glance restarts now (models).
    Restart,
    /// Saved (or not); the window is rebuilt in the new accent.
    Repaint(Result<Applied, String>),
}

/// One section of the window.
pub(crate) trait Section {
    fn name(&self) -> &'static str;
    /// SF Symbol for the sidebar.
    fn symbol(&self) -> &'static str;
    fn view(&self) -> Retained<NSView>;
    fn fill(&mut self, values: &SettingsValues, choices: &SettingsChoices);
    fn on_change(&mut self, id: isize, backend: &dyn SettingsBackend) -> Reply;
    /// Put this section back to `defaults`; `None` when it has nothing to
    /// restore (no button is shown).
    fn restore(
        &mut self,
        _defaults: &SettingsValues,
        _backend: &dyn SettingsBackend,
    ) -> Option<Reply> {
        None
    }
    /// What restoring changes, for the confirmation.
    fn restore_note(&self) -> &'static str {
        "Your other settings stay as they are."
    }
    /// Twice a second while the window is open (model downloads).
    fn tick(&mut self) {}
}

struct Window {
    window: Retained<NSWindow>,
    _target: Retained<Target>,
    sections: Vec<Box<dyn Section>>,
    sidebar: Vec<(Retained<NSBox>, Retained<NSButton>)>,
    title: Retained<NSTextField>,
    restore: Retained<NSButton>,
    scroll: Retained<NSScrollView>,
    status: Retained<NSTextField>,
    current: usize,
}

thread_local! {
    static WINDOW: RefCell<Option<Window>> = const { RefCell::new(None) };
}

/// Every section, in sidebar order.
fn sections(ui: &Ui<'_>) -> Vec<Box<dyn Section>> {
    let mut list: Vec<Box<dyn Section>> = Vec::new();
    let page = |i: usize| Ui {
        mtm: ui.mtm,
        target: ui.target,
        page: i as isize,
    };
    list.push(Box::new(voice::Voice::new(&page(list.len()))));
    list.push(Box::new(answers::Answers::new(&page(list.len()))));
    list.push(Box::new(personality::Personality::new(&page(list.len()))));
    list.push(Box::new(think::Think::new(&page(list.len()))));
    list.push(Box::new(models::Models::new(&page(list.len()))));
    list.push(Box::new(hotkeys::Hotkeys::new(&page(list.len()))));
    list.push(Box::new(appearance::Appearance::new(&page(list.len()))));
    list.push(Box::new(about::About::new(&page(list.len()))));
    list
}

/// Show the window on its first section (tray menu). Main thread.
pub fn open() {
    open_section(None);
}

/// Show the window on the section called `name` ("Models"), or the last
/// one shown.
pub fn open_section(name: Option<&str>) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Some(backend) = crate::shell::settings_backend() else {
        return;
    };
    let (values, choices) = backend.current();
    WINDOW.with_borrow_mut(|window| {
        if window.is_none() {
            *window = Some(Window::build(mtm));
        }
        let Some(w) = window.as_mut() else { return };
        for section in &mut w.sections {
            section.fill(&values, &choices);
        }
        let at = name
            .and_then(|n| w.sections.iter().position(|s| s.name() == n))
            .unwrap_or(w.current);
        w.show(at);
        w.say("");
        if !w.window.isVisible() {
            w.window.center();
        }
        w.window.makeKeyAndOrderFront(None);
    });
    // An accessory app must activate for its window to take typing.
    NSApplication::sharedApplication(mtm).activate();
    start_ticking();
}

fn start_ticking() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        std::thread::spawn(|| {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(500));
                DispatchQueue::main().exec_async(tick);
            }
        });
    });
}

fn tick() {
    let _ = WINDOW.try_with(|cell| {
        // Busy (an alert is up and this ran inside its loop): skip a beat.
        let Ok(mut window) = cell.try_borrow_mut() else {
            return;
        };
        let Some(w) = window.as_mut() else { return };
        if w.window.isVisible() {
            let current = w.current;
            w.sections[current].tick();
        }
    });
}

/// A control changed (from the target). Main thread.
fn on_change(tag: isize) {
    let Some(backend) = crate::shell::settings_backend() else {
        return;
    };
    if (SIDEBAR_TAG..SIDEBAR_TAG + 100).contains(&tag) {
        WINDOW.with_borrow_mut(|w| {
            if let Some(w) = w.as_mut() {
                w.show((tag - SIDEBAR_TAG) as usize);
            }
        });
        return;
    }
    if tag == RESTORE_TAG {
        restore_current(&*backend);
        return;
    }
    let reply = WINDOW.with_borrow_mut(|w| {
        let w = w.as_mut()?;
        let section = w.sections.get_mut((tag / 100) as usize)?;
        Some(section.on_change(tag % 100, &*backend))
    });
    if let Some(reply) = reply {
        show_reply(reply);
    }
}

fn restore_current(backend: &dyn SettingsBackend) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    // Read what to ask, then let go of the window before the alert.
    let Some((name, note)) = WINDOW.with_borrow(|w| {
        w.as_ref().map(|w| {
            let s = &w.sections[w.current];
            (s.name(), s.restore_note())
        })
    }) else {
        return;
    };
    if !confirm(
        mtm,
        &format!(
            "Restore the {} settings to their defaults?",
            name.to_lowercase()
        ),
        note,
        "Restore defaults",
    ) {
        return;
    }
    let defaults = backend.defaults();
    let reply = WINDOW.with_borrow_mut(|w| {
        let w = w.as_mut()?;
        let current = w.current;
        let reply = w.sections[current].restore(&defaults, backend);
        // Show what is saved now.
        let (values, choices) = backend.current();
        w.sections[current].fill(&values, &choices);
        reply
    });
    if let Some(reply) = reply {
        show_reply(reply);
    }
}

fn show_reply(reply: Reply) {
    WINDOW.with_borrow_mut(|w| {
        let Some(w) = w.as_mut() else { return };
        match reply {
            Reply::Nothing => {}
            Reply::Applied(Ok(Applied::Live)) => w.say("Saved."),
            Reply::Applied(Ok(Applied::NeedsRestart)) => {
                w.say("Saved. Restart glance to apply.");
            }
            Reply::Applied(Err(e)) => w.say(&format!("Not saved: {e}")),
            Reply::Say(text) => w.say(&text),
            Reply::Restart => {
                w.say("Saved. Restarting glance…");
                DispatchQueue::main().exec_async(crate::shell::relaunch);
            }
            Reply::Repaint(Err(e)) => w.say(&format!("Not saved: {e}")),
            Reply::Repaint(Ok(_)) => {
                // Controls keep the colour they were given, so build the
                // window again and come back to the same section.
                let name = w.sections[w.current].name();
                DispatchQueue::main().exec_async(move || rebuild(name));
            }
        }
    });
}

fn rebuild(section: &'static str) {
    WINDOW.with_borrow_mut(|w| {
        if let Some(old) = w.take() {
            old.window.orderOut(None);
        }
    });
    open_section(Some(section));
    WINDOW.with_borrow(|w| {
        if let Some(w) = w.as_ref() {
            w.say("Saved.");
        }
    });
}

/// A modal yes/no question; true for `yes`.
pub(crate) fn confirm(mtm: MainThreadMarker, title: &str, text: &str, yes: &str) -> bool {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(text));
    alert.addButtonWithTitle(&NSString::from_str(yes));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    alert.setAlertStyle(NSAlertStyle::Warning);
    alert.runModal() == NSAlertFirstButtonReturn
}

pub(crate) fn symbol(name: &str, description: &str) -> Option<Retained<NSImage>> {
    NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(name),
        Some(&NSString::from_str(description)),
    )
}

impl Window {
    fn build(mtm: MainThreadMarker) -> Self {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(840.0, 640.0));
        // SAFETY: standard initialiser; the window is kept by `WINDOW`.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Miniaturizable
                    | NSWindowStyleMask::Resizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        window.setTitle(&NSString::from_str("glance settings"));
        window.setContentMinSize(NSSize::new(MIN_W, MIN_H));
        // SAFETY: the window is owned by `WINDOW` for the app's lifetime; it
        // must not free itself when closed.
        unsafe { window.setReleasedWhenClosed(false) };
        let content = NSView::initWithFrame(NSView::alloc(mtm), frame);
        window.setContentView(Some(&content));
        // The window background, drawn by the content itself so snapshots
        // (which draw only the content view) match the real window.
        let backdrop = NSBox::new(mtm);
        backdrop.setBoxType(NSBoxType::Custom);
        backdrop.setTitlePosition(NSTitlePosition::NoTitle);
        backdrop.setBorderWidth(0.0);
        backdrop.setFillColor(&NSColor::windowBackgroundColor());
        content.addSubview(&backdrop);
        pin(&backdrop, &content, 0.0);
        let target = Target::new(mtm);
        let ui = Ui {
            mtm,
            target: &target,
            page: 0,
        };
        let sections = sections(&ui);

        // Sidebar.
        let sidebar = NSVisualEffectView::new(mtm);
        sidebar.setMaterial(NSVisualEffectMaterial::Sidebar);
        sidebar.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        content.addSubview(&sidebar);
        auto(&sidebar);
        activate(&[
            sidebar
                .leadingAnchor()
                .constraintEqualToAnchor(&content.leadingAnchor()),
            sidebar
                .topAnchor()
                .constraintEqualToAnchor(&content.topAnchor()),
            sidebar
                .bottomAnchor()
                .constraintEqualToAnchor(&content.bottomAnchor()),
            sidebar.widthAnchor().constraintEqualToConstant(SIDEBAR_W),
        ]);
        let list = stack(mtm, &[], true, 2.0);
        sidebar.addSubview(&list);
        activate(&[
            list.leadingAnchor()
                .constraintEqualToAnchor_constant(&sidebar.leadingAnchor(), 10.0),
            list.trailingAnchor()
                .constraintEqualToAnchor_constant(&sidebar.trailingAnchor(), -10.0),
            list.topAnchor()
                .constraintEqualToAnchor_constant(&sidebar.topAnchor(), 14.0),
        ]);
        let mut items = Vec::new();
        for (i, section) in sections.iter().enumerate() {
            let (cell, button) = sidebar_item(&ui, section.name(), section.symbol(), i);
            list.addArrangedSubview(&cell);
            ui::full_width(&cell, &list, 0.0);
            items.push((cell, button));
        }

        // Page: title row, scrolling content, status line.
        let title = ui::title(mtm, "");
        let restore = ui.button("Restore defaults…", 0, false);
        restore.setTag(RESTORE_TAG);
        let header = NSStackView::new(mtm);
        header.setOrientation(objc2_app_kit::NSUserInterfaceLayoutOrientation::Horizontal);
        header.setDistribution(objc2_app_kit::NSStackViewDistribution::GravityAreas);
        header.addView_inGravity(&title, objc2_app_kit::NSStackViewGravity::Leading);
        header.addView_inGravity(&restore, objc2_app_kit::NSStackViewGravity::Trailing);
        content.addSubview(&header);
        auto(&header);
        let scroll = NSScrollView::new(mtm);
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        scroll.setDrawsBackground(false);
        content.addSubview(&scroll);
        auto(&scroll);
        let status = ui::label(mtm, "");
        status.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        status.setTextColor(Some(&NSColor::secondaryLabelColor()));
        content.addSubview(&status);
        auto(&status);
        let left = sidebar.trailingAnchor();
        activate(&[
            header
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&left, theme::MARGIN),
            header
                .trailingAnchor()
                .constraintEqualToAnchor_constant(&content.trailingAnchor(), -theme::MARGIN),
            header
                .topAnchor()
                .constraintEqualToAnchor_constant(&content.topAnchor(), 18.0),
            scroll.leadingAnchor().constraintEqualToAnchor(&left),
            scroll
                .trailingAnchor()
                .constraintEqualToAnchor(&content.trailingAnchor()),
            scroll
                .topAnchor()
                .constraintEqualToAnchor_constant(&header.bottomAnchor(), 12.0),
            scroll
                .bottomAnchor()
                .constraintEqualToAnchor_constant(&status.topAnchor(), -8.0),
            status
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&left, theme::MARGIN),
            status
                .trailingAnchor()
                .constraintLessThanOrEqualToAnchor_constant(
                    &content.trailingAnchor(),
                    -theme::MARGIN,
                ),
            status
                .bottomAnchor()
                .constraintEqualToAnchor_constant(&content.bottomAnchor(), -12.0),
        ]);
        title.setContentCompressionResistancePriority_forOrientation(
            250.0,
            NSLayoutConstraintOrientation::Horizontal,
        );

        Self {
            window,
            _target: target,
            sections,
            sidebar: items,
            title,
            restore,
            scroll,
            status,
            current: 0,
        }
    }

    fn show(&mut self, at: usize) {
        let at = at.min(self.sections.len() - 1);
        self.current = at;
        let section = &self.sections[at];
        self.title
            .setStringValue(&NSString::from_str(section.name()));
        self.restore.setHidden(section.restore_note().is_empty());
        let page = section.view();
        self.scroll.setDocumentView(Some(&page));
        auto(&page);
        let clip = self.scroll.contentView();
        activate(&[
            page.widthAnchor()
                .constraintEqualToAnchor(&clip.widthAnchor()),
            page.topAnchor().constraintEqualToAnchor(&clip.topAnchor()),
        ]);
        for (i, (cell, button)) in self.sidebar.iter().enumerate() {
            let selected = i == at;
            let fill = if selected {
                theme::accent()
            } else {
                NSColor::clearColor()
            };
            cell.setFillColor(&fill);
            let text = if selected {
                theme::on_accent()
            } else {
                NSColor::labelColor()
            };
            // Borderless buttons tint their title and symbol with this.
            button.setContentTintColor(Some(&text));
        }
        self.say("");
    }

    fn say(&self, text: &str) {
        self.status.setStringValue(&NSString::from_str(text));
    }
}

/// A sidebar entry: a rounded cell (accent when selected) holding a
/// borderless button with the section's symbol and name.
fn sidebar_item(
    ui: &Ui<'_>,
    name: &str,
    symbol_name: &str,
    index: usize,
) -> (Retained<NSBox>, Retained<NSButton>) {
    let mtm = ui.mtm;
    let cell = NSBox::new(mtm);
    cell.setBoxType(NSBoxType::Custom);
    cell.setTitlePosition(NSTitlePosition::NoTitle);
    cell.setCornerRadius(6.0);
    cell.setBorderWidth(0.0);
    cell.setFillColor(&NSColor::clearColor());
    cell.setContentViewMargins(NSSize::new(0.0, 0.0));
    let button = ui.button(name, 0, false);
    button.setTag(SIDEBAR_TAG + index as isize);
    button.setBordered(false);
    button.setImage(symbol(symbol_name, name).as_deref());
    button.setImagePosition(NSCellImagePosition::ImageLeading);
    button.setAlignment(objc2_app_kit::NSTextAlignment::Left);
    button.setImageHugsTitle(true);
    if let Some(content) = cell.contentView() {
        content.addSubview(&button);
        pin(&button, &content, 5.0);
    }
    auto(&cell);
    activate(&[cell.heightAnchor().constraintEqualToConstant(30.0)]);
    (cell, button)
}

/// For `glance ui-snapshots`: build the window, show `section` in the given
/// appearance, and return its content view.
pub(crate) fn stage(
    section: usize,
    dark: bool,
) -> Option<(Retained<NSWindow>, Retained<NSView>, usize)> {
    use objc2_app_kit::NSAppearanceCustomization;
    open_section(None);
    WINDOW.with_borrow_mut(|w| {
        let w = w.as_mut()?;
        w.window
            .setAppearance(crate::snapshot::appearance(dark).as_deref());
        w.show(section);
        w.say("Saved.");
        let count = w.sections.len();
        Some((w.window.clone(), w.window.contentView()?, count))
    })
}

/// The section names, in order (for snapshot file names).
pub(crate) fn section_names() -> Vec<&'static str> {
    WINDOW.with_borrow(|w| {
        w.as_ref()
            .map(|w| w.sections.iter().map(|s| s.name()).collect())
            .unwrap_or_default()
    })
}
