//! The overlay: a small floating panel in the top-right corner showing what
//! glance is doing (a status row with an animated glyph), what the user
//! asked, and the answer as it is spoken. It fades in when a chord is
//! pressed and out a few seconds after the turn ends.
//!
//! Behaviour (unchanged by the M5 redesign):
//! - a borderless `NSPanel` with `NonactivatingPanel`, shown with
//!   `orderFrontRegardless`: it never takes focus, so the app the user is
//!   asking about stays frontmost (and stays the active display);
//! - click-through (`ignoresMouseEvents`), on every Space and over
//!   full-screen apps, at status-window level;
//! - plain `NSTextField` labels only: model output, which comes from
//!   untrusted screen content, is drawn as text, never parsed or linked;
//! - glance's own windows are excluded from screenshots (capture filter).
//!
//! Look: a translucent popover-material background with rounded corners and
//! a hairline border (solid with Reduce Transparency); glyphs in the accent
//! colour, each state with its own shape and label (Listening: a waveform
//! from the live mic level; Thinking: pulsing dots; Speaking: an equaliser).
//! Motion (fades, size changes, glyphs) runs on a ~30 Hz main-queue tick
//! only while the panel is visible, and not at all with Reduce Motion.
//!
//! Main thread only: state lives in a thread-local, and every function here
//! runs on the main thread (through the main dispatch queue).

mod glyph;
pub mod motion;

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dispatch2::DispatchQueue;
use glance_orchestrator::{DeepStatus, Progress, SessionInfo, Shown, TurnState};
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::NSAppearanceCustomization;
use objc2_app_kit::{
    NSBackingStoreType, NSBox, NSBoxType, NSColor, NSFont, NSLineBreakMode, NSPanel, NSScreen,
    NSScrollView, NSStatusWindowLevel, NSTextField, NSTitlePosition, NSView,
    NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView,
    NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use glyph::{Glyph, Shape};

const WIDTH: f64 = 440.0;
/// Distance from the screen's top and right edges.
const MARGIN: f64 = 16.0;
const PADDING: f64 = 16.0;
const STATUS_H: f64 = 20.0;
const GAP: f64 = 10.0;
/// The answer scrolls (kept at its end) beyond this share of the screen.
const MAX_SCREEN_SHARE: f64 = 0.4;
/// How long the overlay stays after a turn ends.
const HIDE_AFTER: Duration = Duration::from_secs(6);
/// The answer shown is cut to its last characters beyond this.
const MAX_REPLY_CHARS: usize = 900;
const FADE_IN: f64 = 0.15;
const FADE_OUT: f64 = 0.25;
const RESIZE: f64 = 0.12;
/// How often the mic level is sampled for the waveform.
const LEVEL_EVERY: Duration = Duration::from_millis(70);

/// A change for the overlay. Built on any thread, applied on the main one.
#[derive(Debug, Clone)]
pub enum OverlayEvent {
    State(TurnState),
    Progress(Progress),
    Deep(DeepStatus),
    Session(SessionInfo),
    Shown(Shown),
}

struct Fade {
    from: f64,
    to: f64,
    started: Instant,
    duration: f64,
}

struct Overlay {
    panel: Retained<NSPanel>,
    blur: Retained<NSVisualEffectView>,
    solid: Retained<NSBox>,
    border: Retained<NSBox>,
    glyph: Retained<Glyph>,
    status: Retained<NSTextField>,
    meta: Retained<NSTextField>,
    asked: Retained<NSTextField>,
    scroll: Retained<NSScrollView>,
    body: Retained<NSTextField>,
    turn_state: TurnState,
    progress: Option<Progress>,
    deep: Option<DeepStatus>,
    question: String,
    reply: String,
    screenshot_taken: Option<Instant>,
    hide_at: Option<Instant>,
    /// Shown, or fading in; false once a fade-out has started.
    visible: bool,
    fade: Option<Fade>,
    /// The question's laid-out height (0 when there is none).
    asked_h: f64,
    /// Height now, and the size change under way: (from, to, started).
    height: f64,
    resize: Option<(f64, f64, Instant)>,
    /// Recent mic levels, newest last.
    levels: [f32; 5],
    last_level: Instant,
    epoch: Instant,
    reduce_motion: bool,
    /// `glance ui-snapshots`: draw the glyphs at this time, levels fixed.
    frozen: Option<f64>,
}

thread_local! {
    static OVERLAY: RefCell<Option<Overlay>> = const { RefCell::new(None) };
}

/// The frame tick runs while this is set.
static ANIMATING: AtomicBool = AtomicBool::new(false);

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

    let root = NSView::initWithFrame(NSView::alloc(mtm), frame);
    panel.setContentView(Some(&root));

    let blur = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), frame);
    blur.setMaterial(NSVisualEffectMaterial::Popover);
    blur.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    blur.setState(NSVisualEffectState::Active);
    blur.setMaskImage(Some(&crate::theme::rounded_mask(
        crate::theme::PANEL_RADIUS,
    )));
    root.addSubview(&blur);

    // Reduce Transparency: a solid system background instead of the blur.
    let solid = rounded_box(mtm, &NSColor::windowBackgroundColor(), 0.0);
    solid.setHidden(true);
    root.addSubview(&solid);
    // The hairline border on top of either.
    let border = rounded_box(mtm, &NSColor::clearColor(), 1.0);
    root.addSubview(&border);

    let glyph = Glyph::new(mtm);
    let status = NSTextField::labelWithString(&NSString::from_str(""), mtm);
    status.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
    status.setTextColor(Some(&NSColor::labelColor()));
    status.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    let meta = NSTextField::labelWithString(&NSString::from_str(""), mtm);
    meta.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    meta.setTextColor(Some(&NSColor::secondaryLabelColor()));
    meta.setAlignment(objc2_app_kit::NSTextAlignment::Right);
    meta.setLineBreakMode(NSLineBreakMode::ByTruncatingHead);

    // The question: quieter than the answer, at most two lines.
    let asked = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
    asked.setFont(Some(&NSFont::systemFontOfSize(13.0)));
    asked.setTextColor(Some(&NSColor::secondaryLabelColor()));
    asked.setSelectable(false);
    asked.setMaximumNumberOfLines(2);
    asked.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    asked.setPreferredMaxLayoutWidth(WIDTH - 2.0 * PADDING);

    let body = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
    body.setFont(Some(&NSFont::systemFontOfSize(15.0)));
    body.setTextColor(Some(&NSColor::labelColor()));
    body.setSelectable(false);
    body.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
    body.setPreferredMaxLayoutWidth(WIDTH - 2.0 * PADDING);
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
    scroll.setDrawsBackground(false);
    scroll.setHasVerticalScroller(false);
    scroll.setDocumentView(Some(&body));

    for view in [&*glyph as &NSView, &status, &meta, &asked, &scroll] {
        root.addSubview(view);
    }

    OVERLAY.with_borrow_mut(|overlay| {
        *overlay = Some(Overlay {
            panel,
            blur,
            solid,
            border,
            glyph,
            status,
            meta,
            asked,
            scroll,
            body,
            turn_state: TurnState::Idle,
            progress: None,
            deep: None,
            question: String::new(),
            reply: String::new(),
            screenshot_taken: None,
            hide_at: None,
            visible: false,
            fade: None,
            asked_h: 0.0,
            height: 0.0,
            resize: None,
            levels: [0.0; 5],
            last_level: Instant::now(),
            epoch: Instant::now(),
            reduce_motion: false,
            frozen: None,
        });
    });
}

fn rounded_box(mtm: MainThreadMarker, fill: &NSColor, border: f64) -> Retained<NSBox> {
    let b = NSBox::new(mtm);
    b.setBoxType(NSBoxType::Custom);
    b.setTitlePosition(NSTitlePosition::NoTitle);
    b.setCornerRadius(crate::theme::PANEL_RADIUS);
    b.setBorderWidth(border);
    b.setBorderColor(&NSColor::separatorColor());
    b.setFillColor(fill);
    b.setContentViewMargins(NSSize::new(0.0, 0.0));
    b
}

/// Apply a change (main thread).
pub fn apply(event: OverlayEvent) {
    OVERLAY.with_borrow_mut(|overlay| {
        let Some(o) = overlay.as_mut() else { return };
        match event {
            OverlayEvent::State(state) => {
                o.turn_state = state;
                if state != TurnState::Thinking {
                    o.progress = None;
                }
                if state == TurnState::Idle {
                    o.hide_at = Some(Instant::now() + HIDE_AFTER);
                } else {
                    o.hide_at = None;
                    o.show();
                }
            }
            OverlayEvent::Progress(progress) => o.progress = Some(progress),
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

/// Every few seconds: start hiding when the time is up, refresh the
/// screenshot age.
pub fn tick() {
    OVERLAY.with_borrow_mut(|overlay| {
        let Some(o) = overlay.as_mut() else { return };
        if o.hide_at.is_some_and(|at| Instant::now() >= at) {
            o.hide_at = None;
            o.hide();
            return;
        }
        if o.visible {
            o.render();
        }
    });
}

/// Run `frame` about 30 times a second until the overlay is hidden.
fn start_animating() {
    if ANIMATING.swap(true, Ordering::Relaxed) {
        return;
    }
    std::thread::spawn(|| {
        while ANIMATING.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(33));
            DispatchQueue::main().exec_async(frame);
        }
    });
}

fn frame() {
    let _ = OVERLAY.try_with(|cell| {
        let Ok(mut overlay) = cell.try_borrow_mut() else {
            return;
        };
        let Some(o) = overlay.as_mut() else { return };
        o.animate();
    });
}

/// `glance ui-snapshots`: lay the overlay out in a fixed state (solid
/// background, since in-process rendering draws no blur) and return its
/// content view and panel, to be drawn into an image.
pub(crate) fn stage(
    state: TurnState,
    progress: Option<Progress>,
    question: &str,
    reply: &str,
    levels: [f32; 5],
    dark: bool,
) -> Option<(Retained<NSPanel>, Retained<NSView>)> {
    OVERLAY.with_borrow_mut(|overlay| {
        let o = overlay.as_mut()?;
        o.panel
            .setAppearance(crate::snapshot::appearance(dark).as_deref());
        o.turn_state = state;
        o.progress = progress;
        o.question = question.to_owned();
        o.reply = reply.to_owned();
        o.screenshot_taken = Some(Instant::now() - Duration::from_secs(42));
        o.levels = levels;
        o.frozen = Some(0.4);
        o.reduce_motion = true;
        o.solid.setHidden(false);
        o.blur.setHidden(true);
        o.height = 0.0;
        o.render();
        Some((o.panel.clone(), o.panel.contentView()?))
    })
}

fn reduce_motion() -> bool {
    NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

fn reduce_transparency() -> bool {
    NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceTransparency()
}

impl Overlay {
    fn show(&mut self) {
        self.reduce_motion = reduce_motion();
        let solid = reduce_transparency();
        self.solid.setHidden(!solid);
        self.blur.setHidden(solid);
        if !self.visible {
            self.visible = true;
            let from = if self.panel.isVisible() {
                self.panel.alphaValue()
            } else {
                0.0
            };
            if self.reduce_motion {
                self.panel.setAlphaValue(1.0);
                self.fade = None;
            } else {
                self.panel.setAlphaValue(from);
                self.fade = Some(Fade {
                    from,
                    to: 1.0,
                    started: Instant::now(),
                    duration: FADE_IN,
                });
            }
            self.panel.orderFrontRegardless();
        }
        start_animating();
    }

    fn hide(&mut self) {
        if !self.visible {
            return;
        }
        self.visible = false;
        if self.reduce_motion {
            self.panel.orderOut(None);
            ANIMATING.store(false, Ordering::Relaxed);
            return;
        }
        self.fade = Some(Fade {
            from: self.panel.alphaValue(),
            to: 0.0,
            started: Instant::now(),
            duration: FADE_OUT,
        });
        start_animating();
    }

    /// One frame: fade, size change, glyph.
    fn animate(&mut self) {
        if let Some(fade) = &self.fade {
            let t = fade.started.elapsed().as_secs_f64();
            self.panel
                .setAlphaValue(motion::tween(fade.from, fade.to, t, fade.duration));
            if t >= fade.duration {
                let gone = fade.to == 0.0;
                self.fade = None;
                if gone {
                    self.panel.orderOut(None);
                    ANIMATING.store(false, Ordering::Relaxed);
                    return;
                }
            }
        }
        if let Some((from, to, started)) = self.resize {
            let t = started.elapsed().as_secs_f64();
            self.height = motion::tween(from, to, t, RESIZE);
            if t >= RESIZE {
                self.resize = None;
            }
            self.place();
        }
        if self.last_level.elapsed() >= LEVEL_EVERY {
            self.last_level = Instant::now();
            self.levels.rotate_left(1);
            self.levels[4] = if self.turn_state == TurnState::Listening {
                crate::shell::mic_level()
            } else {
                0.0
            };
        }
        self.glyph.set_shape(self.shape());
        if !self.visible && self.fade.is_none() {
            ANIMATING.store(false, Ordering::Relaxed);
        }
    }

    fn shape(&self) -> Shape {
        let t = self
            .frozen
            .unwrap_or_else(|| self.epoch.elapsed().as_secs_f64());
        let still = self.reduce_motion;
        match self.turn_state {
            TurnState::Idle => Shape::Empty,
            TurnState::Listening if still => Shape::Wave(motion::STILL_LISTENING),
            TurnState::Listening => Shape::Wave(motion::listening_bars(&self.levels)),
            TurnState::Transcribing | TurnState::Thinking if still => {
                Shape::Dots(motion::STILL_THINKING)
            }
            TurnState::Transcribing | TurnState::Thinking => Shape::Dots(motion::thinking_dots(t)),
            TurnState::Speaking if still => Shape::Equaliser(motion::STILL_SPEAKING),
            TurnState::Speaking => Shape::Equaliser(motion::speaking_bars(t)),
        }
    }

    /// Text and layout after a change.
    fn render(&mut self) {
        self.status
            .setStringValue(&NSString::from_str(&self.status_text()));
        self.meta
            .setStringValue(&NSString::from_str(&self.meta_text()));
        let asked = if self.question.is_empty() {
            String::new()
        } else {
            format!("“{}”", self.question)
        };
        self.asked.setStringValue(&NSString::from_str(&asked));
        let body = tail(&self.reply, MAX_REPLY_CHARS);
        self.body.setStringValue(&NSString::from_str(&body));
        self.glyph.set_shape(self.shape());

        let inner = WIDTH - 2.0 * PADDING;
        self.asked_h = if asked.is_empty() {
            0.0
        } else {
            self.asked.sizeThatFits(NSSize::new(inner, 1000.0)).height
        };
        let body_h = if body.is_empty() {
            0.0
        } else {
            self.body.sizeThatFits(NSSize::new(inner, 10_000.0)).height
        };
        let max_body = self.max_body_height();
        let shown_body = body_h.min(max_body);
        self.body.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(inner, body_h),
        ));
        // Keep the end of the answer in view (the document is not flipped,
        // so its end is at y = 0).
        self.body.scrollPoint(NSPoint::new(0.0, 0.0));
        let asked_block = if self.asked_h > 0.0 {
            GAP + self.asked_h
        } else {
            0.0
        };
        let body_block = if body.is_empty() {
            0.0
        } else {
            GAP + shown_body
        };
        let target = (PADDING + STATUS_H + asked_block + body_block + PADDING).ceil();
        if self.height == 0.0 || self.reduce_motion || !self.panel.isVisible() {
            self.height = target;
            self.resize = None;
        } else if (target - self.height).abs() >= 1.0
            && self.resize.is_none_or(|(_, to, _)| to != target)
        {
            self.resize = Some((self.height, target, Instant::now()));
            start_animating();
        }
        self.place();
    }

    fn max_body_height(&self) -> f64 {
        let Some(mtm) = MainThreadMarker::new() else {
            return 400.0;
        };
        NSScreen::mainScreen(mtm).map_or(400.0, |screen| {
            screen.visibleFrame().size.height * MAX_SCREEN_SHARE
        })
    }

    /// Position the panel and lay out its contents for `self.height`.
    fn place(&self) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let height = self.height;
        let inner = WIDTH - 2.0 * PADDING;
        let all = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, height));
        for view in [&*self.blur as &NSView, &self.solid, &self.border] {
            view.setFrame(all);
        }
        let status_y = height - PADDING - STATUS_H;
        self.glyph.setFrame(NSRect::new(
            NSPoint::new(PADDING, status_y + (STATUS_H - glyph::HEIGHT) / 2.0),
            NSSize::new(glyph::WIDTH, glyph::HEIGHT),
        ));
        let text_x = PADDING + glyph::WIDTH + 10.0;
        let meta_w = 170.0;
        self.status.setFrame(NSRect::new(
            NSPoint::new(text_x, status_y + 1.0),
            NSSize::new(WIDTH - PADDING - text_x - meta_w - 8.0, 18.0),
        ));
        self.meta.setFrame(NSRect::new(
            NSPoint::new(WIDTH - PADDING - meta_w, status_y + 2.0),
            NSSize::new(meta_w, 16.0),
        ));
        let mut below = status_y;
        if self.asked_h > 0.0 {
            below -= GAP + self.asked_h;
            self.asked.setFrame(NSRect::new(
                NSPoint::new(PADDING, below),
                NSSize::new(inner, self.asked_h),
            ));
        }
        self.asked.setHidden(self.asked_h == 0.0);
        let body_h = (below - GAP - PADDING).max(0.0);
        self.scroll.setFrame(NSRect::new(
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
        self.panel.invalidateShadow();
    }

    fn status_text(&self) -> String {
        if let (TurnState::Thinking, Some(progress)) = (self.turn_state, self.progress) {
            return match progress {
                Progress::LoadingDeep => "Loading the bigger model…",
                Progress::ReadingScreen => "Reading the screen…",
                Progress::ThinkingHarder => "Thinking harder…",
            }
            .to_owned();
        }
        state_text(self.turn_state).to_owned()
    }

    fn meta_text(&self) -> String {
        let mut parts = Vec::new();
        if self.deep == Some(DeepStatus::Loading) && self.progress != Some(Progress::LoadingDeep) {
            parts.push("loading the bigger model".to_owned());
        }
        if let Some(taken) = self.screenshot_taken {
            parts.push(format!("screenshot {}", age(taken.elapsed())));
        }
        parts.join(" · ")
    }
}

fn state_text(state: TurnState) -> &'static str {
    match state {
        TurnState::Idle => "Ready",
        TurnState::Listening => "Listening",
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

    #[test]
    fn every_state_has_its_own_label() {
        let states = [
            TurnState::Idle,
            TurnState::Listening,
            TurnState::Transcribing,
            TurnState::Thinking,
            TurnState::Speaking,
        ];
        let labels: std::collections::HashSet<_> = states.iter().map(|s| state_text(*s)).collect();
        assert_eq!(labels.len(), states.len());
    }
}
