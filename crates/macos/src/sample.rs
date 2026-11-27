//! The built-in sample image for setup's test question: a small bar chart
//! drawn by glance itself (the same numbers as the overlay demo), rendered
//! to PNG bytes in memory. The real screen is never captured for the test,
//! and the image is never written to disk.
#![allow(unsafe_code)]

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAppearanceCustomization, NSBox, NSBoxType, NSColor, NSFont, NSTextAlignment, NSTextField,
    NSTitlePosition, NSView,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

/// Weekly active users, weeks 1 to 4.
pub const WEEKS: [(&str, u32); 4] = [
    ("Week 1", 1240),
    ("Week 2", 1450),
    ("Week 3", 1390),
    ("Week 4", 1720),
];

const W: f64 = 1024.0;
const H: f64 = 640.0;

fn text(
    mtm: MainThreadMarker,
    s: &str,
    size: f64,
    bold: bool,
    frame: NSRect,
) -> Retained<NSTextField> {
    let l = NSTextField::labelWithString(&NSString::from_str(s), mtm);
    let font = if bold {
        NSFont::boldSystemFontOfSize(size)
    } else {
        NSFont::systemFontOfSize(size)
    };
    l.setFont(Some(&font));
    l.setTextColor(Some(&NSColor::blackColor()));
    l.setAlignment(NSTextAlignment::Center);
    l.setFrame(frame);
    l
}

fn rect(mtm: MainThreadMarker, colour: &NSColor, frame: NSRect) -> Retained<NSBox> {
    let b = NSBox::new(mtm);
    b.setBoxType(NSBoxType::Custom);
    b.setTitlePosition(NSTitlePosition::NoTitle);
    b.setBorderWidth(0.0);
    b.setFillColor(colour);
    b.setFrame(frame);
    b
}

fn thousands(n: u32) -> String {
    if n >= 1000 {
        format!("{},{:03}", n / 1000, n % 1000)
    } else {
        n.to_string()
    }
}

/// The chart as a view (light appearance, white background).
pub(crate) fn chart_view(mtm: MainThreadMarker) -> Retained<NSView> {
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(W, H));
    let root = NSView::initWithFrame(NSView::alloc(mtm), frame);
    root.setAppearance(crate::snapshot::appearance(false).as_deref());
    root.addSubview(&rect(mtm, &NSColor::whiteColor(), frame));
    let at = |x: f64, y: f64, w: f64, h: f64| NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
    root.addSubview(&text(
        mtm,
        "Weekly active users",
        34.0,
        true,
        at(0.0, H - 80.0, W, 44.0),
    ));
    root.addSubview(&text(
        mtm,
        "Last four weeks",
        20.0,
        false,
        at(0.0, H - 116.0, W, 28.0),
    ));
    let base = 110.0;
    let top = H - 190.0;
    let max = WEEKS.iter().map(|(_, v)| *v).max().unwrap_or(1) as f64;
    let bar_w = 150.0;
    let gap = (W - 4.0 * bar_w) / 5.0;
    let bar = NSColor::colorWithSRGBRed_green_blue_alpha(0.85, 0.35, 0.19, 1.0);
    root.addSubview(&rect(
        mtm,
        &NSColor::blackColor(),
        at(gap * 0.5, base - 2.0, W - gap, 2.0),
    ));
    for (i, (week, value)) in WEEKS.iter().enumerate() {
        let x = gap + i as f64 * (bar_w + gap);
        let h = (top - base) * f64::from(*value) / max;
        root.addSubview(&rect(mtm, &bar, at(x, base, bar_w, h)));
        root.addSubview(&text(
            mtm,
            &thousands(*value),
            24.0,
            true,
            at(x - 20.0, base + h + 8.0, bar_w + 40.0, 32.0),
        ));
        root.addSubview(&text(
            mtm,
            week,
            22.0,
            false,
            at(x - 20.0, base - 44.0, bar_w + 40.0, 30.0),
        ));
    }
    root.addSubview(&text(
        mtm,
        "Week 3: onboarding email paused",
        18.0,
        false,
        at(0.0, 20.0, W, 26.0),
    ));
    root
}

/// The chart as PNG bytes, in memory. Main thread.
pub fn chart_png() -> Result<Vec<u8>, String> {
    let mtm = MainThreadMarker::new().ok_or("the sample chart is drawn on the main thread")?;
    crate::snapshot::png_of(&chart_view(mtm))
}

#[cfg(test)]
mod tests {
    #[test]
    fn numbers_read_like_the_demo() {
        assert_eq!(super::thousands(1240), "1,240");
        assert_eq!(super::thousands(1720), "1,720");
        assert_eq!(super::thousands(999), "999");
    }
}
