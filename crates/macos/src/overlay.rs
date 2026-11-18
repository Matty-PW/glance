//! The overlay: a small floating panel in the top-right corner showing what
//! glance is doing (status line), what the user asked, and the answer as it
//! is spoken. It appears when a chord is pressed and hides a few seconds
//! after the turn ends.
//!
//! - A borderless `NSPanel` with `NonactivatingPanel`, shown with
//!   `orderFrontRegardless`: it never takes focus, so the app the user is
//!   asking about stays frontmost (and stays the active display).
//! - Click-through (`ignoresMouseEvents`), on every Space and over
//!   full-screen apps, at status-window level.
//! - Plain `NSTextField` labels only: model output, which comes from
//!   untrusted screen content, is drawn as text, never parsed or linked.
//! - glance's own windows are excluded from screenshots (capture filter).
//!
//! Main thread only: state lives in a thread-local, and every function here
//! runs on the main thread (through the main dispatch queue).

use std::cell::RefCell;
use std::time::{Duration, Instant};

use glance_orchestrator::{DeepStatus, Progress, SessionInfo, Shown, TurnState};
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSFont, NSLineBreakMode, NSPanel, NSScreen, NSStatusWindowLevel,
    NSTextField, NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState,
    NSVisualEffectView, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

const WIDTH: f64 = 420.0;
/// Distance from the screen's top and right edges.
const MARGIN: f64 = 16.0;
const PADDING: f64 = 14.0;
const GAP: f64 = 6.0;
/// How long the overlay stays after a turn ends.
const HIDE_AFTER: Duration = Duration::from_secs(6);
/// The answer shown is cut to its last characters beyond this.
const MAX_REPLY_CHARS: usize = 900;

/// A change for the overlay. Built on any thread, applied on the main one.
#[derive(Debug, Clone)]
pub enum OverlayEvent {
    State(TurnState),
    Progress(Progress),
    Deep(DeepStatus),
    Session(SessionInfo),
    Shown(Shown),
}

struct Overlay {
    panel: Retained<NSPanel>,
    status: Retained<NSTextField>,
    body: Retained<NSTextField>,
    state: String,
    deep: Option<DeepStatus>,
    question: String,
    reply: String,
    screenshot_taken: Option<std::time::Instant>,
    hide_at: Option<Instant>,
}

thread_local! {
    static OVERLAY: RefCell<Option<Overlay>> = const { RefCell::new(None) };
}

/// Create the (hidden) panel. Main thread, after the run loop started.
pub fn install() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, 80.0));
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        frame,
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
        NSBackingStoreType::Buffered,
        false,
    );
    panel.setLevel(NSStatusWindowLevel);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    panel.setIgnoresMouseEvents(true);
    panel.setFloatingPanel(true);
    panel.setBecomesKeyOnlyIfNeeded(true);
    panel.setHidesOnDeactivate(false);
    panel.setOpaque(false);
    panel.setHasShadow(true);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));

    let effect = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), frame);
    effect.setMaterial(NSVisualEffectMaterial::HUDWindow);
    effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    effect.setState(NSVisualEffectState::Active);
    panel.setContentView(Some(&effect));

    let status = NSTextField::labelWithString(&NSString::from_str(""), mtm);
    status.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    status.setTextColor(Some(&NSColor::secondaryLabelColor()));
    let body = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
    body.setFont(Some(&NSFont::systemFontOfSize(15.0)));
    body.setTextColor(Some(&NSColor::labelColor()));
    body.setSelectable(false);
    body.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
    body.setPreferredMaxLayoutWidth(WIDTH - 2.0 * PADDING);
    effect.addSubview(&status);
    effect.addSubview(&body);

    OVERLAY.with_borrow_mut(|overlay| {
        *overlay = Some(Overlay {
            panel,
            status,
            body,
            state: String::new(),
            deep: None,
            question: String::new(),
            reply: String::new(),
            screenshot_taken: None,
            hide_at: None,
        });
    });
}

/// Apply a change (main thread).
pub fn apply(event: OverlayEvent) {
    OVERLAY.with_borrow_mut(|overlay| {
        let Some(o) = overlay.as_mut() else { return };
        match event {
            OverlayEvent::State(state) => {
                o.state = state_text(state).to_owned();
                if state == TurnState::Idle {
                    o.hide_at = Some(Instant::now() + HIDE_AFTER);
                } else {
                    o.hide_at = None;
                    o.panel.orderFrontRegardless();
                }
            }
            OverlayEvent::Progress(progress) => {
                o.state = match progress {
                    Progress::LoadingDeep => "Loading the bigger model…",
                    Progress::ReadingScreen => "Reading the screen…",
                    Progress::ThinkingHarder => "Thinking harder…",
                }
                .to_owned();
            }
            OverlayEvent::Deep(status) => o.deep = Some(status),
            OverlayEvent::Session(info) => o.screenshot_taken = info.screenshot_taken,
            OverlayEvent::Shown(Shown::Clear) => {
                o.question.clear();
                o.reply.clear();
            }
            OverlayEvent::Shown(Shown::Asked(question)) => o.question = question,
            OverlayEvent::Shown(Shown::Said(sentence)) => {
                if !o.reply.is_empty() {
                    o.reply.push(' ');
                }
                o.reply.push_str(&sentence);
            }
        }
        o.render();
    });
}

/// Every few seconds: hide when the time is up, refresh the screenshot age.
pub fn tick() {
    OVERLAY.with_borrow_mut(|overlay| {
        let Some(o) = overlay.as_mut() else { return };
        if o.hide_at.is_some_and(|at| Instant::now() >= at) {
            o.hide_at = None;
            o.panel.orderOut(None);
            return;
        }
        o.render();
    });
}

impl Overlay {
    fn render(&self) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        self.status
            .setStringValue(&NSString::from_str(&self.status_line()));
        let body = self.body_text();
        self.body.setStringValue(&NSString::from_str(&body));

        // Lay out by hand: status on top, the text under it.
        let inner = WIDTH - 2.0 * PADDING;
        let status_h = self.status.sizeThatFits(NSSize::new(inner, 1000.0)).height;
        let body_h = if body.is_empty() {
            0.0
        } else {
            self.body.sizeThatFits(NSSize::new(inner, 10_000.0)).height
        };
        let gap = if body.is_empty() { 0.0 } else { GAP };
        let height = PADDING + status_h + gap + body_h + PADDING;
        self.status.setFrame(NSRect::new(
            NSPoint::new(PADDING, height - PADDING - status_h),
            NSSize::new(inner, status_h),
        ));
        self.body.setFrame(NSRect::new(
            NSPoint::new(PADDING, PADDING),
            NSSize::new(inner, body_h),
        ));

        let Some(screen) = NSScreen::mainScreen(mtm) else {
            return;
        };
        let visible = screen.visibleFrame();
        let origin = NSPoint::new(
            visible.origin.x + visible.size.width - WIDTH - MARGIN,
            visible.origin.y + visible.size.height - height - MARGIN,
        );
        self.panel
            .setFrame_display(NSRect::new(origin, NSSize::new(WIDTH, height)), true);
    }

    fn status_line(&self) -> String {
        let mut parts = vec![if self.state.is_empty() {
            "glance".to_owned()
        } else {
            self.state.clone()
        }];
        if self.deep == Some(DeepStatus::Loading) && !self.state.starts_with("Loading") {
            parts.push("loading the bigger model".into());
        }
        if let Some(taken) = self.screenshot_taken {
            parts.push(format!("screenshot {}", age(taken.elapsed())));
        }
        parts.join(" · ")
    }

    fn body_text(&self) -> String {
        let reply = tail(&self.reply, MAX_REPLY_CHARS);
        match (self.question.is_empty(), reply.is_empty()) {
            (true, true) => String::new(),
            (false, true) => format!("“{}”", self.question),
            (true, false) => reply,
            (false, false) => format!("“{}”\n\n{reply}", self.question),
        }
    }
}

fn state_text(state: TurnState) -> &'static str {
    match state {
        TurnState::Idle => "Ready",
        TurnState::Listening => "Listening…",
        TurnState::Transcribing => "Transcribing…",
        TurnState::Thinking => "Thinking…",
        TurnState::Speaking => "Speaking",
    }
}

fn age(elapsed: Duration) -> String {
    match elapsed.as_secs() {
        0..10 => "just taken".into(),
        s @ 10..60 => format!("{s} s old"),
        s => format!("{} min old", s / 60),
    }
}

/// The last `max` characters, with an ellipsis when cut.
fn tail(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_owned();
    }
    let cut: String = text.chars().skip(count - max).collect();
    format!("…{}", cut.trim_start())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_answers_keep_their_end() {
        assert_eq!(tail("short", 10), "short");
        let long = "word ".repeat(400);
        let cut = tail(&long, 100);
        assert!(cut.starts_with('…'));
        assert!(cut.chars().count() <= 101);
    }

    #[test]
    fn screenshot_age_reads_naturally() {
        assert_eq!(age(Duration::from_secs(3)), "just taken");
        assert_eq!(age(Duration::from_secs(42)), "42 s old");
        assert_eq!(age(Duration::from_secs(130)), "2 min old");
    }
}
