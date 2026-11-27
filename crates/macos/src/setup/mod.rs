//! The first-launch setup window: one step per page (Welcome, Your Mac,
//! Permissions, Download models, Try it, Done, or a short "You're all
//! set"), progress dots, Back and Next. In the settings window's style:
//! the same rounded groups and rows, glance's accent, semantic colours for
//! light and dark, Auto Layout, no animation and no blur (so Reduce Motion
//! and Reduce Transparency have nothing to switch off).
//!
//! The window shows plain data from the app's [`SetupBackend`]; the
//! permission states come from [`perms`]. Closing it is always allowed
//! (a download carries on), Cmd-Q quits, and "Skip setup" never marks setup
//! complete while models are missing (the backend decides).
//!
//! Main thread only.
#![allow(unsafe_code)]

mod pages;
pub(crate) mod perms;

use std::cell::RefCell;

use dispatch2::DispatchQueue;
use glance_orchestrator::setup::{Facts, Flow, SetupBackend, SetupSnapshot, Skip, Step};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAlert, NSApplication, NSBackingStoreType, NSBox, NSBoxType, NSButton, NSColor, NSControl,
    NSEventModifierFlags, NSFont, NSImageView, NSScrollView, NSStackView, NSTextField,
    NSTitlePosition, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use crate::settings::ui::{self, Ui, activate, auto, pin};
use crate::theme;
use pages::{Env, StepPage};
use perms::Permissions;

const W: f64 = 700.0;
const H: f64 = 620.0;
const MIN_W: f64 = 620.0;
const MIN_H: f64 = 520.0;

const BACK: isize = 9_000;
const NEXT: isize = 9_001;
const SKIP: isize = 9_002;
const FINISH: isize = 9_003;
const SETTINGS: isize = 9_004;
const QUIT: isize = 9_005;
const CLOSE: isize = 9_006;
const FULL: isize = 9_007;

/// What a page asks the window to do.
pub(crate) enum Reply {
    Nothing,
    /// Read the backend again now.
    Refresh,
    Say(String),
}

/// What a page's handler can use.
pub(crate) struct Ctx<'a> {
    pub snap: &'a SetupSnapshot,
    pub backend: &'a dyn SetupBackend,
    pub perms: &'a mut dyn Permissions,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `Target` has no state and does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "GlanceSetupWindowTarget"]
    struct Target;

    unsafe impl NSObjectProtocol for Target {}

    impl Target {
        #[unsafe(method(changed:))]
        fn changed(&self, sender: &NSControl) {
            let tag = sender.tag();
            // Not inside the action: alerts and the backend run after it.
            DispatchQueue::main().exec_async(move || on_change(tag));
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

struct Window {
    window: Retained<NSWindow>,
    _target: Retained<Target>,
    pages: Vec<Box<dyn StepPage>>,
    flow: Flow,
    perms: Box<dyn Permissions>,
    dots: Vec<Retained<NSImageView>>,
    dots_row: Retained<NSStackView>,
    position: Retained<NSTextField>,
    title: Retained<NSTextField>,
    scroll: Retained<NSScrollView>,
    status: Retained<NSTextField>,
    back: Retained<NSButton>,
    next: Retained<NSButton>,
    skip: Retained<NSButton>,
    finish: Retained<NSButton>,
    settings: Retained<NSButton>,
    full: Retained<NSButton>,
    shown: Option<Step>,
    snap: Option<SetupSnapshot>,
}

thread_local! {
    static WINDOW: RefCell<Option<Window>> = const { RefCell::new(None) };
}

/// The page index of a step (pages are built in this order).
fn page_of(step: Step) -> usize {
    step.index().unwrap_or(Step::FLOW.len())
}

fn backend() -> Option<std::sync::Arc<dyn SetupBackend>> {
    crate::shell::setup_backend()
}

/// Show the window at `step` (Welcome when `None`: "Run setup again").
/// Main thread.
pub fn open(step: Option<Step>) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let perms: Box<dyn Permissions> = if crate::shell::setup_demo() {
        Box::new(perms::Demo::new())
    } else {
        Box::new(perms::Real::new())
    };
    open_with(mtm, step.unwrap_or(Step::Welcome), perms);
    // An accessory app must activate for its window to take clicks and keys.
    NSApplication::sharedApplication(mtm).activate();
    start_ticking();
}

fn open_with(mtm: MainThreadMarker, step: Step, perms: Box<dyn Permissions>) {
    WINDOW.with_borrow_mut(|window| {
        if window.is_none() {
            *window = Some(Window::build(mtm, perms));
        }
        let Some(w) = window.as_mut() else { return };
        w.flow.go(step);
        w.refresh();
        if !w.window.isVisible() {
            w.window.center();
        }
        w.window.makeKeyAndOrderFront(None);
    });
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
        if let Some(w) = window.as_mut()
            && w.window.isVisible()
        {
            w.refresh();
        }
    });
}

fn on_change(tag: isize) {
    let Some(backend) = backend() else { return };
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    match tag {
        QUIT => {
            crate::shell::quit();
            return;
        }
        CLOSE => {
            close();
            return;
        }
        SKIP => {
            skip(mtm, &*backend);
            return;
        }
        _ => {}
    }
    let reply = WINDOW.with_borrow_mut(|w| {
        let w = w.as_mut()?;
        let snap = w.snap.clone().unwrap_or_else(|| backend.snapshot());
        let facts = Facts {
            models_ready: snap.models_ready,
        };
        match tag {
            BACK => {
                w.flow.back();
                Some(Reply::Refresh)
            }
            NEXT => {
                w.flow.next(facts);
                Some(Reply::Refresh)
            }
            FULL => {
                w.flow.go(Step::Welcome);
                Some(Reply::Refresh)
            }
            SETTINGS => {
                DispatchQueue::main().exec_async(crate::settings::open);
                Some(Reply::Nothing)
            }
            FINISH => Some(finish(w, &*backend, &snap)),
            tag => {
                let page = w.pages.get_mut((tag / 100) as usize)?;
                let mut ctx = Ctx {
                    snap: &snap,
                    backend: &*backend,
                    perms: &mut *w.perms,
                };
                Some(page.on_change(tag % 100, &mut ctx))
            }
        }
    });
    WINDOW.with_borrow_mut(|w| {
        let Some(w) = w.as_mut() else { return };
        match reply {
            Some(Reply::Refresh) => {
                w.say("");
                w.refresh();
            }
            Some(Reply::Say(text)) => {
                w.refresh();
                w.say(&text);
            }
            _ => {}
        }
    });
}

fn finish(w: &mut Window, backend: &dyn SetupBackend, snap: &SetupSnapshot) -> Reply {
    if let Err(e) = backend.finish() {
        return Reply::Say(format!("Not yet: {e}."));
    }
    w.window.orderOut(None);
    if snap.needs_restart {
        crate::shell::restart();
    }
    Reply::Nothing
}

fn skip(mtm: MainThreadMarker, backend: &dyn SetupBackend) {
    match backend.skip() {
        Skip::MarkComplete => {}
        Skip::CloseOnly => {
            let alert = NSAlert::new(mtm);
            alert.setMessageText(&NSString::from_str("Setup is not finished"));
            alert.setInformativeText(&NSString::from_str(
                "The models are not downloaded yet, so glance cannot answer. Setup opens again \
                 the next time glance starts, or from the menu-bar menu.",
            ));
            alert.addButtonWithTitle(&NSString::from_str("OK"));
            alert.runModal();
        }
    }
    close();
}

fn close() {
    WINDOW.with_borrow(|w| {
        if let Some(w) = w.as_ref() {
            w.window.orderOut(None);
        }
    });
}

impl Window {
    fn build(mtm: MainThreadMarker, perms: Box<dyn Permissions>) -> Self {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(W, H));
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
        window.setTitle(&NSString::from_str("Set up glance"));
        window.setContentMinSize(NSSize::new(MIN_W, MIN_H));
        // SAFETY: owned by `WINDOW` for the app's lifetime.
        unsafe { window.setReleasedWhenClosed(false) };
        let content = NSView::initWithFrame(NSView::alloc(mtm), frame);
        window.setContentView(Some(&content));
        let backdrop = NSBox::new(mtm);
        backdrop.setBoxType(NSBoxType::Custom);
        backdrop.setTitlePosition(NSTitlePosition::NoTitle);
        backdrop.setBorderWidth(0.0);
        backdrop.setFillColor(&NSColor::windowBackgroundColor());
        content.addSubview(&backdrop);
        pin(&backdrop, &content, 0.0);

        let target = Target::new(mtm);
        let any: &AnyObject = &target;
        let page_ui = |i: usize| Ui {
            mtm,
            target: any,
            page: i as isize,
        };
        let pages: Vec<Box<dyn StepPage>> = vec![
            Box::new(pages::Welcome::new(&page_ui(0))),
            Box::new(pages::YourMac::new(&page_ui(1))),
            Box::new(pages::PermissionsPage::new(&page_ui(2))),
            Box::new(pages::Download::new(&page_ui(3))),
            Box::new(pages::TryIt::new(&page_ui(4))),
            Box::new(pages::Done::new(&page_ui(5))),
            Box::new(pages::AllSet::new(&page_ui(6))),
        ];
        let ui = page_ui(0);

        // Header: dots, "Step 2 of 6", title.
        let dots: Vec<Retained<NSImageView>> = Step::FLOW
            .iter()
            .map(|_| {
                let v = NSImageView::new(mtm);
                auto(&v);
                activate(&[
                    v.widthAnchor().constraintEqualToConstant(16.0),
                    v.heightAnchor().constraintEqualToConstant(16.0),
                ]);
                v
            })
            .collect();
        let refs: Vec<&NSView> = dots.iter().map(|d| -> &NSView { d }).collect();
        let dots_row = ui::stack(mtm, &refs, false, 10.0);
        let position = ui::label(mtm, "");
        position.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        position.setTextColor(Some(&NSColor::secondaryLabelColor()));
        let title = ui::title(mtm, "");
        for v in [&*dots_row as &NSView, &position, &title] {
            content.addSubview(v);
            auto(v);
        }

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

        // Footer: Skip on the left; the step's buttons on the right.
        let skip = ui.button("Skip setup", 0, false);
        skip.setTag(SKIP);
        let back = ui.button("Back", 0, false);
        back.setTag(BACK);
        let next = ui.button("Next", 0, true);
        next.setTag(NEXT);
        next.setKeyEquivalent(&NSString::from_str("\r"));
        let full = ui.button("Run full setup", 0, false);
        full.setTag(FULL);
        let settings = ui.button("Open Settings", 0, false);
        settings.setTag(SETTINGS);
        let finish = ui.button("Start using glance", 0, true);
        finish.setTag(FINISH);
        // Cmd-Q and Cmd-W work although an accessory app has no menu bar:
        // two borderless, title-less buttons carry the key equivalents.
        let quit = ui.button("", 0, false);
        quit.setTag(QUIT);
        quit.setBordered(false);
        quit.setKeyEquivalent(&NSString::from_str("q"));
        quit.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
        let close_key = ui.button("", 0, false);
        close_key.setTag(CLOSE);
        close_key.setBordered(false);
        close_key.setKeyEquivalent(&NSString::from_str("w"));
        close_key.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
        let footer = NSStackView::new(mtm);
        footer.setOrientation(objc2_app_kit::NSUserInterfaceLayoutOrientation::Horizontal);
        footer.setDistribution(objc2_app_kit::NSStackViewDistribution::GravityAreas);
        footer.setSpacing(8.0);
        footer.addView_inGravity(&skip, objc2_app_kit::NSStackViewGravity::Leading);
        footer.addView_inGravity(&quit, objc2_app_kit::NSStackViewGravity::Leading);
        footer.addView_inGravity(&close_key, objc2_app_kit::NSStackViewGravity::Leading);
        for b in [&back, &full, &settings, &next, &finish] {
            footer.addView_inGravity(b, objc2_app_kit::NSStackViewGravity::Trailing);
        }
        content.addSubview(&footer);
        auto(&footer);
        let line = NSBox::new(mtm);
        line.setBoxType(NSBoxType::Separator);
        content.addSubview(&line);
        auto(&line);
        for b in [&quit, &close_key] {
            activate(&[
                b.widthAnchor().constraintEqualToConstant(1.0),
                b.heightAnchor().constraintEqualToConstant(1.0),
            ]);
        }

        let m = theme::MARGIN;
        activate(&[
            dots_row
                .topAnchor()
                .constraintEqualToAnchor_constant(&content.topAnchor(), 18.0),
            dots_row
                .centerXAnchor()
                .constraintEqualToAnchor(&content.centerXAnchor()),
            position
                .topAnchor()
                .constraintEqualToAnchor_constant(&dots_row.bottomAnchor(), 6.0),
            position
                .centerXAnchor()
                .constraintEqualToAnchor(&content.centerXAnchor()),
            title
                .topAnchor()
                .constraintEqualToAnchor_constant(&position.bottomAnchor(), 10.0),
            title
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&content.leadingAnchor(), m),
            title
                .trailingAnchor()
                .constraintLessThanOrEqualToAnchor_constant(&content.trailingAnchor(), -m),
            scroll
                .topAnchor()
                .constraintEqualToAnchor_constant(&title.bottomAnchor(), 10.0),
            scroll
                .leadingAnchor()
                .constraintEqualToAnchor(&content.leadingAnchor()),
            scroll
                .trailingAnchor()
                .constraintEqualToAnchor(&content.trailingAnchor()),
            scroll
                .bottomAnchor()
                .constraintEqualToAnchor_constant(&status.topAnchor(), -6.0),
            status
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&content.leadingAnchor(), m),
            status
                .trailingAnchor()
                .constraintLessThanOrEqualToAnchor_constant(&content.trailingAnchor(), -m),
            status
                .bottomAnchor()
                .constraintEqualToAnchor_constant(&line.topAnchor(), -8.0),
            line.leadingAnchor()
                .constraintEqualToAnchor(&content.leadingAnchor()),
            line.trailingAnchor()
                .constraintEqualToAnchor(&content.trailingAnchor()),
            line.bottomAnchor()
                .constraintEqualToAnchor_constant(&footer.topAnchor(), -12.0),
            footer
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&content.leadingAnchor(), m),
            footer
                .trailingAnchor()
                .constraintEqualToAnchor_constant(&content.trailingAnchor(), -m),
            footer
                .bottomAnchor()
                .constraintEqualToAnchor_constant(&content.bottomAnchor(), -16.0),
        ]);

        Self {
            window,
            _target: target,
            pages,
            flow: Flow::new(Step::Welcome),
            perms,
            dots,
            dots_row,
            position,
            title,
            scroll,
            status,
            back,
            next,
            skip,
            finish,
            settings,
            full,
            shown: None,
            snap: None,
        }
    }

    /// Read the backend and the permissions, and show the current step.
    fn refresh(&mut self) {
        let Some(backend) = backend() else { return };
        let snap = backend.snapshot();
        let env = Env {
            screen: self.perms.screen(snap.screen_asked),
            mic: self.perms.mic(),
            signing: self.perms.signing(),
        };
        let step = self.flow.step();
        if self.shown != Some(step) {
            self.show_page(step);
            backend.save_step(step);
        }
        self.pages[page_of(step)].fill(&snap, &env);
        self.update_chrome(step, &snap);
        self.snap = Some(snap);
    }

    fn show_page(&mut self, step: Step) {
        self.shown = Some(step);
        let page = self.pages[page_of(step)].view();
        self.scroll.setDocumentView(Some(&page));
        auto(&page);
        let clip = self.scroll.contentView();
        activate(&[
            page.widthAnchor()
                .constraintEqualToAnchor(&clip.widthAnchor()),
            page.topAnchor().constraintEqualToAnchor(&clip.topAnchor()),
        ]);
        self.title.setStringValue(&NSString::from_str(step.title()));
        self.say("");
    }

    fn update_chrome(&self, step: Step, snap: &SetupSnapshot) {
        let facts = Facts {
            models_ready: snap.models_ready,
        };
        // Dots: a check for done steps, a filled ring for this one, an
        // empty circle for the rest; shape and size carry the meaning, the
        // accent only adds to it.
        let at = step.index();
        self.dots_row.setHidden(at.is_none());
        for (i, dot) in self.dots.iter().enumerate() {
            let (symbol, word, tint) = match at {
                Some(now) if i < now => (
                    "checkmark.circle.fill",
                    "done",
                    NSColor::secondaryLabelColor(),
                ),
                Some(now) if i == now => ("smallcircle.filled.circle", "current", theme::accent()),
                _ => ("circle", "to do", NSColor::tertiaryLabelColor()),
            };
            let description = format!("{}: {word}", Step::FLOW[i].title());
            dot.setImage(crate::settings::symbol(symbol, &description).as_deref());
            dot.setContentTintColor(Some(&tint));
            dot.setToolTip(Some(&NSString::from_str(&description)));
        }
        self.position.setStringValue(&NSString::from_str(
            &self
                .flow
                .position()
                .map_or(String::new(), |(n, of)| format!("Step {n} of {of}")),
        ));
        let last = matches!(step, Step::Done | Step::AllSet);
        self.back.setHidden(!self.flow.can_back());
        self.next.setHidden(last);
        self.next.setEnabled(self.flow.can_next(facts));
        self.skip.setHidden(last);
        self.finish.setHidden(!last);
        self.finish.setEnabled(self.flow.can_finish(facts));
        self.finish
            .setTitle(&NSString::from_str(if snap.needs_restart {
                "Restart and start using glance"
            } else {
                "Start using glance"
            }));
        self.settings.setHidden(!last);
        self.full.setHidden(step != Step::AllSet);
        self.next.setToolTip(
            (step == Step::Download && !facts.models_ready)
                .then(|| NSString::from_str("Available once the models are downloaded."))
                .as_deref(),
        );
    }

    fn say(&self, text: &str) {
        self.status.setStringValue(&NSString::from_str(text));
    }
}

/// For `glance ui-snapshots`: the window at `step` with fixed permission
/// states, in the given appearance; returns its content view.
pub(crate) fn stage(
    step: Step,
    dark: bool,
    perms: Box<dyn Permissions>,
    advanced_open: bool,
) -> Option<(Retained<NSWindow>, Retained<NSView>)> {
    use objc2_app_kit::NSAppearanceCustomization;
    let mtm = MainThreadMarker::new()?;
    WINDOW.with_borrow_mut(|w| {
        if let Some(old) = w.take() {
            old.window.orderOut(None);
        }
    });
    open_with(mtm, step, perms);
    WINDOW.with_borrow_mut(|w| {
        let w = w.as_mut()?;
        w.window
            .setAppearance(crate::snapshot::appearance(dark).as_deref());
        if advanced_open {
            w.pages[page_of(Step::YourMac)].open_advanced();
        }
        w.refresh();
        Some((w.window.clone(), w.window.contentView()?))
    })
}

/// For `glance ui-snapshots`: the smallest window, or the usual size.
pub(crate) fn set_minimum_size(on: bool) {
    WINDOW.with_borrow(|w| {
        if let Some(w) = w.as_ref() {
            w.window.setContentSize(if on {
                NSSize::new(MIN_W, MIN_H)
            } else {
                NSSize::new(W, H)
            });
        }
    });
}
