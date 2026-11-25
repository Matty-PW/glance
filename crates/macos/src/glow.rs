//! The screenshot indicator: a short glow around the edge of the display
//! glance has just captured, in the accent colour, fading out in about half
//! a second. `[appearance] screenshot_indicator` picks Glow, Subtle (a thin
//! line) or Off; it applies live.
//!
//! - It never appears in a screenshot: it is shown only after the capture
//!   has returned (the orchestrator sends `ScreenshotUse::Fresh` then), and
//!   its panel is one of glance's windows, which the capture filter leaves
//!   out (`capture.rs`, the same mechanism as the overlay).
//! - The panel is the overlay's kind: borderless and non-activating, never
//!   key, click-through, on every Space and over full-screen apps.
//! - It costs nearly nothing: four thin edge views are drawn once, and the
//!   fade is a Core Animation opacity animation (`NSAnimationContext` and
//!   the animator on layer-backed views), run by the render server with no
//!   per-frame work here. Between flashes the panel is off screen.
//! - Reduce Motion: a still line that fades once, no pulse. Reduce
//!   Transparency: an opaque line, no translucent band. Both are read at
//!   each flash. The overlay also says in words that a screenshot is new,
//!   so the glow is never the only signal.
//!
//! Main thread only.
#![allow(unsafe_code)]

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchTime};
use objc2::rc::Retained;
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAnimatablePropertyContainer, NSAnimationContext, NSBackingStoreType, NSBezierPath, NSColor,
    NSGradient, NSPanel, NSScreen, NSStatusWindowLevel, NSView, NSWindowAnimationBehavior,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSNumber, NSPoint, NSRect, NSSize, NSString};

use crate::theme;

/// What marks a new screenshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Indicator {
    Glow,
    Subtle,
    Off,
}

impl Indicator {
    /// The config value ("glow", "subtle", "off").
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "glow" => Some(Self::Glow),
            "subtle" => Some(Self::Subtle),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

/// How a flash is drawn and timed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    /// Depth of the soft band fading inward from the edge, in points (0:
    /// no band).
    pub band: f64,
    /// The line along the very edge, in points.
    pub line: f64,
    /// The line's opacity (1.0 with Reduce Transparency).
    pub line_alpha: f64,
    pub timing: Timing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timing {
    /// A quick rise and a softer fade.
    Pulse,
    /// Reduce Motion: shown at once, held, then one plain fade.
    Still,
}

/// The band's opacity at the edge.
const BAND_ALPHA: f64 = 0.55;
const RISE: f64 = 0.08;
const FALL: f64 = 0.42;
const HOLD: Duration = Duration::from_millis(500);
const STILL_FADE: f64 = 0.2;

/// The look for a setting and the accessibility options (`None`: Off).
pub fn look(indicator: Indicator, reduce_motion: bool, reduce_transparency: bool) -> Option<Look> {
    let timing = if reduce_motion {
        Timing::Still
    } else {
        Timing::Pulse
    };
    let line_alpha = if reduce_transparency { 1.0 } else { 0.9 };
    match indicator {
        Indicator::Off => None,
        // The full glow: a translucent band, unless transparency or motion
        // should be kept down; then a clear, slightly heavier line.
        Indicator::Glow if !reduce_motion && !reduce_transparency => Some(Look {
            band: 36.0,
            line: 1.5,
            line_alpha,
            timing,
        }),
        Indicator::Glow => Some(Look {
            band: 0.0,
            line: 3.0,
            line_alpha,
            timing,
        }),
        Indicator::Subtle => Some(Look {
            band: 0.0,
            line: 2.0,
            line_alpha: if reduce_transparency { 1.0 } else { 0.85 },
            timing,
        }),
    }
}

/// The looks `--overlay-demo` tours, with a caption each.
pub fn demo_looks() -> [(&'static str, Look); 4] {
    let get = |i, motion, transparency| look(i, motion, transparency).expect("not Off");
    [
        ("Glow", get(Indicator::Glow, false, false)),
        ("Subtle", get(Indicator::Subtle, false, false)),
        ("Glow with Reduce Motion", get(Indicator::Glow, true, false)),
        (
            "Glow with Reduce Transparency",
            get(Indicator::Glow, false, true),
        ),
    ]
}

static INDICATOR: AtomicU8 = AtomicU8::new(0);

/// Use this setting from the next flash on ("glow", "subtle", "off";
/// anything else is Glow, the default).
pub fn set_indicator(name: &str) {
    let value = match Indicator::parse(name).unwrap_or(Indicator::Glow) {
        Indicator::Glow => 0,
        Indicator::Subtle => 1,
        Indicator::Off => 2,
    };
    INDICATOR.store(value, Ordering::Relaxed);
}

pub fn indicator() -> Indicator {
    match INDICATOR.load(Ordering::Relaxed) {
        1 => Indicator::Subtle,
        2 => Indicator::Off,
        _ => Indicator::Glow,
    }
}

/// Which edge a view runs along.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

const EDGES: [Edge; 4] = [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right];

/// Each edge's frame in a view of `size`, `depth` points deep. Top and
/// bottom span the width; left and right the height (they overlap at the
/// corners, which only makes the corners a little brighter).
fn edge_frames(size: NSSize, depth: f64) -> [NSRect; 4] {
    let depth = depth.min(size.width / 2.0).min(size.height / 2.0);
    EDGES.map(|edge| match edge {
        Edge::Top => NSRect::new(
            NSPoint::new(0.0, size.height - depth),
            NSSize::new(size.width, depth),
        ),
        Edge::Bottom => NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(size.width, depth)),
        Edge::Left => NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(depth, size.height)),
        Edge::Right => NSRect::new(
            NSPoint::new(size.width - depth, 0.0),
            NSSize::new(depth, size.height),
        ),
    })
}

/// Index of the screen showing `display` among `ids` (each screen's
/// display id, if it has one), or `None` to use the main screen.
fn screen_index(ids: &[Option<u32>], display: Option<u32>) -> Option<usize> {
    let display = display?;
    ids.iter().position(|id| *id == Some(display))
}

pub struct Ivars {
    edge: Cell<Edge>,
    look: Cell<Look>,
}

define_class!(
    // SAFETY:
    // - NSView has no subclassing requirements beyond calling its
    //   designated initialiser, which `new` does.
    // - `EdgeView` does not implement `Drop`.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "GlanceScreenshotEdge"]
    #[ivars = Ivars]
    pub struct EdgeView;

    impl EdgeView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }
    }
);

impl EdgeView {
    fn new(mtm: MainThreadMarker, edge: Edge) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            edge: Cell::new(edge),
            look: Cell::new(look(Indicator::Glow, false, false).expect("glow")),
        });
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0));
        // SAFETY: NSView's designated initialiser on a fresh allocation.
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        // Layer-backed: drawn once into its layer, then only composited.
        view.setWantsLayer(true);
        view
    }

    fn set_look(&self, look: Look) {
        if self.ivars().look.get() != look {
            self.ivars().look.set(look);
            self.setNeedsDisplay(true);
        }
    }

    /// The band (accent at the edge, fading to nothing inward), then the
    /// line along the very edge.
    fn draw(&self) {
        let look = self.ivars().look.get();
        let edge = self.ivars().edge.get();
        let b = self.bounds();
        let accent = theme::accent();
        if look.band > 0.0 {
            // Fading inward: the start colour sits on the outer edge.
            let angle = match edge {
                Edge::Top => 270.0,
                Edge::Bottom => 90.0,
                Edge::Left => 0.0,
                Edge::Right => 180.0,
            };
            if let Some(gradient) = NSGradient::initWithStartingColor_endingColor(
                NSGradient::alloc(),
                &accent.colorWithAlphaComponent(BAND_ALPHA),
                &accent.colorWithAlphaComponent(0.0),
            ) {
                gradient.drawInRect_angle(b, angle);
            }
        }
        let line = look.line.min(b.size.width).min(b.size.height);
        let rect = match edge {
            Edge::Top => NSRect::new(
                NSPoint::new(0.0, b.size.height - line),
                NSSize::new(b.size.width, line),
            ),
            Edge::Bottom => NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(b.size.width, line)),
            Edge::Left => NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(line, b.size.height)),
            Edge::Right => NSRect::new(
                NSPoint::new(b.size.width - line, 0.0),
                NSSize::new(line, b.size.height),
            ),
        };
        accent.colorWithAlphaComponent(look.line_alpha).setFill();
        NSBezierPath::fillRect(rect);
    }
}

struct Glow {
    panel: Retained<NSPanel>,
    /// Holds the edges; its opacity is what animates.
    root: Retained<NSView>,
    edges: [Retained<EdgeView>; 4],
}

thread_local! {
    static GLOW: RefCell<Option<Glow>> = const { RefCell::new(None) };
    /// Bumped by every flash, so an older flash's completion never hides a
    /// newer one.
    static GENERATION: Cell<u64> = const { Cell::new(0) };
}

/// A new screenshot was taken of `display`: flash with the current setting
/// (nothing when it is Off).
pub fn flash(display: Option<u32>) {
    if let Some(look) = look(
        indicator(),
        theme::reduce_motion(),
        theme::reduce_transparency(),
    ) {
        play(display, look);
    }
}

/// Flash `look` whatever the setting (Settings' preview, the demo).
pub fn preview(display: Option<u32>, look: Look) {
    play(display, look);
}

/// Show `look` at full strength and keep it there, with no animation,
/// until [`hide`] (the exclusion check, the snapshots).
pub fn hold(display: Option<u32>, look: Look) {
    if show(display, look).is_some() {
        with_glow(|glow| glow.root.setAlphaValue(1.0));
    }
}

/// Take the glow off screen at once.
pub fn hide() {
    next_generation();
    with_glow(|glow| glow.panel.orderOut(None));
}

/// The display id of the screen showing the key window, or the main
/// screen (Settings' preview).
pub fn key_display() -> Option<u32> {
    let mtm = MainThreadMarker::new()?;
    NSScreen::mainScreen(mtm).and_then(|screen| display_id(&screen))
}

fn play(display: Option<u32>, look: Look) {
    let Some(generation) = show(display, look) else {
        return;
    };
    match look.timing {
        Timing::Pulse => {
            with_glow(|glow| glow.root.setAlphaValue(0.0));
            fade(1.0, RISE, move || {
                if current(generation) {
                    fade(0.0, FALL, move || finish(generation));
                }
            });
        }
        Timing::Still => {
            with_glow(|glow| glow.root.setAlphaValue(1.0));
            let when = DispatchTime::try_from(HOLD).unwrap_or(DispatchTime::NOW);
            let _ = DispatchQueue::main().after(when, move || {
                if current(generation) {
                    fade(0.0, STILL_FADE, move || finish(generation));
                }
            });
        }
    }
}

/// Put the panel on `display`'s screen with `look`, in front; returns this
/// flash's generation.
fn show(display: Option<u32>, look: Look) -> Option<u64> {
    let mtm = MainThreadMarker::new()?;
    let screen = screen_for(mtm, display)?;
    let generation = next_generation();
    GLOW.with_borrow_mut(|glow| {
        let glow = glow.get_or_insert_with(|| install(mtm));
        let frame = screen.frame();
        glow.panel.setFrame_display(frame, false);
        let size = frame.size;
        glow.root
            .setFrame(NSRect::new(NSPoint::new(0.0, 0.0), size));
        let depth = look.band.max(look.line);
        for (view, rect) in glow.edges.iter().zip(edge_frames(size, depth)) {
            view.setFrame(rect);
            view.set_look(look);
        }
        glow.panel.orderFrontRegardless();
    });
    Some(generation)
}

fn install(mtm: MainThreadMarker) -> Glow {
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, 100.0));
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        frame,
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
        NSBackingStoreType::Buffered,
        false,
    );
    // As the overlay: above full-screen apps and the menu bar, on every
    // Space, out of Mission Control and the window cycle, never focused.
    panel.setLevel(NSStatusWindowLevel);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    panel.setIgnoresMouseEvents(true);
    panel.setFloatingPanel(true);
    panel.setBecomesKeyOnlyIfNeeded(true);
    panel.setHidesOnDeactivate(false);
    panel.setOpaque(false);
    panel.setHasShadow(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    // No zoom-in when it appears: the fade is the only motion.
    panel.setAnimationBehavior(NSWindowAnimationBehavior::None);

    let content = NSView::initWithFrame(NSView::alloc(mtm), frame);
    content.setWantsLayer(true);
    panel.setContentView(Some(&content));
    let root = NSView::initWithFrame(NSView::alloc(mtm), frame);
    root.setWantsLayer(true);
    content.addSubview(&root);
    let edges = EDGES.map(|edge| EdgeView::new(mtm, edge));
    for edge in &edges {
        root.addSubview(edge);
    }
    Glow { panel, root, edges }
}

/// Animate the edges' opacity to `to` over `seconds`, then `done`.
fn fade(to: f64, seconds: f64, done: impl Fn() + 'static) {
    let Some(root) = GLOW.with_borrow(|glow| glow.as_ref().map(|g| g.root.clone())) else {
        return;
    };
    let changes = RcBlock::new(move |context: NonNull<NSAnimationContext>| {
        // SAFETY: AppKit passes the current context for the call's duration.
        let context = unsafe { context.as_ref() };
        context.setDuration(seconds);
        root.animator().setAlphaValue(to);
    });
    let done = RcBlock::new(done);
    NSAnimationContext::runAnimationGroup_completionHandler(&changes, Some(&done));
}

/// The flash ended: hide the panel unless a newer flash took over.
fn finish(generation: u64) {
    if current(generation) {
        with_glow(|glow| glow.panel.orderOut(None));
    }
}

fn next_generation() -> u64 {
    GENERATION.with(|g| {
        g.set(g.get() + 1);
        g.get()
    })
}

fn current(generation: u64) -> bool {
    GENERATION.with(|g| g.get() == generation)
}

fn with_glow(f: impl FnOnce(&Glow)) {
    GLOW.with_borrow(|glow| {
        if let Some(glow) = glow {
            f(glow);
        }
    });
}

fn display_id(screen: &NSScreen) -> Option<u32> {
    let key = NSString::from_str("NSScreenNumber");
    let value = screen.deviceDescription().objectForKey(&key)?;
    value
        .downcast::<NSNumber>()
        .ok()
        .map(|n| n.unsignedIntValue())
}

/// The screen showing `display`, else the main screen.
fn screen_for(mtm: MainThreadMarker, display: Option<u32>) -> Option<Retained<NSScreen>> {
    let screens = NSScreen::screens(mtm);
    let ids: Vec<Option<u32>> = screens.iter().map(|s| display_id(&s)).collect();
    match screen_index(&ids, display) {
        Some(i) => Some(screens.objectAtIndex(i)),
        None => NSScreen::mainScreen(mtm),
    }
}

// ---- exclusion check (`glance capture-check --indicator`) -------------------

/// Edge differences (mean per channel, 0–255) between a capture with no
/// glow and captures with the glow held on screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExclusionCheck {
    /// The capture glance uses (glance's windows excluded).
    pub excluded: f64,
    /// A control capture with glance's windows left in.
    pub control: f64,
}

/// What the numbers mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The glow was on screen (the control saw it) and the real capture
    /// did not see it.
    Pass,
    /// The real capture saw something at the edges.
    Fail,
    /// The control did not see the glow either, so this run proves nothing
    /// (the glow never reached the screen, or the edges match the accent).
    Inconclusive,
}

/// Above this, the edges changed visibly.
pub const SEEN: f64 = 10.0;
/// Below this, the edges are unchanged (allowing a little screen noise).
pub const UNCHANGED: f64 = 1.5;

impl ExclusionCheck {
    pub fn verdict(&self) -> Verdict {
        if self.control <= SEEN {
            Verdict::Inconclusive
        } else if self.excluded < UNCHANGED {
            Verdict::Pass
        } else {
            Verdict::Fail
        }
    }
}

/// Prove the glow is not in glance's screenshots: capture `screen`'s
/// active display with no glow (baseline), hold the full glow on it,
/// capture again as glance does (glance excluded) and once with glance's
/// windows left in (control), then compare the outer `band` pixels of each
/// with the baseline. Only numbers come out; nothing is saved. Main thread;
/// needs Screen Recording permission.
pub fn exclusion_check(
    screen: &crate::capture::LiveScreen,
    band: u32,
) -> Result<ExclusionCheck, String> {
    use glance_capture::ScreenCapture;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use objc2_foundation::{NSDate, NSRunLoop};

    let mtm = MainThreadMarker::new().ok_or("must run on the main thread")?;
    NSApplication::sharedApplication(mtm)
        .setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let settle = |seconds: f64| {
        NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(seconds));
    };
    let fail = |e: glance_capture::CaptureError| e.to_string();

    let baseline = screen.capture().map_err(fail)?;
    let display = baseline
        .display()
        .ok_or("the capture did not name its display")?;
    let full = look(Indicator::Glow, false, false).expect("glow");
    hold(Some(display), full);
    // Let the window server put the panel on screen.
    settle(0.4);
    let excluded = screen.capture();
    let control = screen.capture_including_glance_for_check(display);
    hide();
    settle(0.1);
    let (excluded, control) = (excluded.map_err(fail)?, control.map_err(fail)?);
    let differ = |shot: &glance_capture::Screenshot| {
        baseline
            .edge_difference(shot, band)
            .ok_or_else(|| "captures differ in size".to_owned())
    };
    Ok(ExclusionCheck {
        excluded: differ(&excluded)?,
        control: differ(&control)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_check_passes_only_when_the_control_saw_the_glow() {
        let check = |excluded, control| ExclusionCheck { excluded, control }.verdict();
        assert_eq!(check(0.2, 60.0), Verdict::Pass);
        assert_eq!(check(12.0, 60.0), Verdict::Fail);
        assert_eq!(check(1.5, 60.0), Verdict::Fail);
        // The control saw nothing: the run says nothing either way.
        assert_eq!(check(0.0, 0.3), Verdict::Inconclusive);
        assert_eq!(check(40.0, 2.0), Verdict::Inconclusive);
    }

    #[test]
    fn off_never_shows_and_glow_is_the_full_band() {
        for motion in [false, true] {
            for transparency in [false, true] {
                assert_eq!(look(Indicator::Off, motion, transparency), None);
            }
        }
        let glow = look(Indicator::Glow, false, false).unwrap();
        assert!(glow.band > 0.0);
        assert_eq!(glow.timing, Timing::Pulse);
    }

    #[test]
    fn reduce_motion_never_pulses_and_drops_the_band() {
        for indicator in [Indicator::Glow, Indicator::Subtle] {
            for transparency in [false, true] {
                let l = look(indicator, true, transparency).unwrap();
                assert_eq!(l.timing, Timing::Still);
                assert_eq!(l.band, 0.0);
            }
        }
    }

    #[test]
    fn reduce_transparency_is_an_opaque_line_with_no_band() {
        for indicator in [Indicator::Glow, Indicator::Subtle] {
            for motion in [false, true] {
                let l = look(indicator, motion, true).unwrap();
                assert_eq!(l.band, 0.0);
                assert_eq!(l.line_alpha, 1.0);
                assert!(l.line >= 2.0);
            }
        }
    }

    #[test]
    fn subtle_is_only_a_thin_line() {
        let l = look(Indicator::Subtle, false, false).unwrap();
        assert_eq!(l.band, 0.0);
        assert!(l.line <= 2.0);
        assert_eq!(l.timing, Timing::Pulse);
    }

    #[test]
    fn the_setting_parses_and_unknown_values_mean_glow() {
        assert_eq!(Indicator::parse("glow"), Some(Indicator::Glow));
        assert_eq!(Indicator::parse("subtle"), Some(Indicator::Subtle));
        assert_eq!(Indicator::parse("off"), Some(Indicator::Off));
        assert_eq!(Indicator::parse("Glow"), None);
        set_indicator("off");
        assert_eq!(indicator(), Indicator::Off);
        set_indicator("subtle");
        assert_eq!(indicator(), Indicator::Subtle);
        set_indicator("nonsense");
        assert_eq!(indicator(), Indicator::Glow);
    }

    #[test]
    fn the_glow_goes_on_the_captured_display() {
        let ids = [Some(1), Some(69_733_632), None];
        assert_eq!(screen_index(&ids, Some(69_733_632)), Some(1));
        assert_eq!(screen_index(&ids, Some(1)), Some(0));
        // Unknown (unplugged) or not given: the main screen.
        assert_eq!(screen_index(&ids, Some(5)), None);
        assert_eq!(screen_index(&ids, None), None);
        assert_eq!(screen_index(&[], Some(1)), None);
    }

    #[test]
    fn edges_hug_the_sides_and_fit_small_screens() {
        let size = NSSize::new(1512.0, 982.0);
        let [top, bottom, left, right] = edge_frames(size, 36.0);
        assert_eq!(top.origin.y + top.size.height, size.height);
        assert_eq!(top.size.width, size.width);
        assert_eq!(bottom.origin.y, 0.0);
        assert_eq!(left.origin.x, 0.0);
        assert_eq!(left.size.height, size.height);
        assert_eq!(right.origin.x + right.size.width, size.width);
        for rect in [top, bottom, left, right] {
            assert_eq!(rect.size.width.min(rect.size.height), 36.0);
        }
        // Never deeper than half the screen.
        let [top, ..] = edge_frames(NSSize::new(40.0, 20.0), 36.0);
        assert_eq!(top.size.height, 10.0);
    }
}
