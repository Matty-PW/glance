//! glance's look in one place: the accent colour (the only colour values in
//! this crate), corner radii and spacing. Everything else uses semantic
//! system colours (label, secondary label, separator, window and control
//! backgrounds), so light, dark and high-contrast modes work by themselves.
//!
//! To change the accent, edit [`ACCENT_LIGHT`] / [`ACCENT_DARK`]; the tests
//! below re-check contrast for the new values.
//!
//! The accent goes only where AppKit lets an app set it: the overlay's
//! state glyphs, the selected sidebar item, slider fills, primary buttons
//! and keycaps. Checkboxes, switches and focus rings always follow the
//! macOS accent (an app needs an asset catalog, i.e. full Xcode, to change
//! those).
#![allow(unsafe_code)]

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_app_kit::{NSAppearance, NSBezierPath, NSColor, NSImage, NSImageResizingMode};
use objc2_foundation::{NSEdgeInsets, NSRect, NSSize};

/// The accent in light mode: a warm orange-coral.
pub const ACCENT_LIGHT: u32 = 0xD85A30;
/// The accent in dark mode: a little lighter, for contrast on dark grey.
pub const ACCENT_DARK: u32 = 0xEE7A50;

/// Corner radius of the overlay panel and of grouped settings boxes.
pub const PANEL_RADIUS: f64 = 14.0;
pub const GROUP_RADIUS: f64 = 10.0;
/// Page margin, gap between groups, and padding inside a group.
pub const MARGIN: f64 = 24.0;
pub const GROUP_GAP: f64 = 18.0;
pub const GROUP_PADDING: f64 = 14.0;

/// `[appearance] accent = "system"`: use the macOS accent instead.
static SYSTEM_ACCENT: AtomicBool = AtomicBool::new(false);

pub fn set_system_accent(on: bool) {
    SYSTEM_ACCENT.store(on, Ordering::Relaxed);
}

pub fn uses_system_accent() -> bool {
    SYSTEM_ACCENT.load(Ordering::Relaxed)
}

/// System Settings > Accessibility > Display > Reduce motion.
pub(crate) fn reduce_motion() -> bool {
    objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

/// System Settings > Accessibility > Display > Reduce transparency.
pub(crate) fn reduce_transparency() -> bool {
    objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceTransparency()
}

fn srgb(rgb: u32) -> Retained<NSColor> {
    let channel = |shift: u32| f64::from((rgb >> shift) & 0xFF) / 255.0;
    NSColor::colorWithSRGBRed_green_blue_alpha(channel(16), channel(8), channel(0), 1.0)
}

/// Whether `appearance` is a dark one (Dark Aqua, vibrant dark, or their
/// high-contrast versions).
pub fn is_dark(appearance: &NSAppearance) -> bool {
    appearance.name().to_string().contains("Dark")
}

/// The accent: glance's (switching with light and dark by itself) or, with
/// the setting on "system", the macOS accent. Fetch it again after the
/// setting changes.
pub fn accent() -> Retained<NSColor> {
    if uses_system_accent() {
        return NSColor::controlAccentColor();
    }
    let light = srgb(ACCENT_LIGHT);
    let dark = srgb(ACCENT_DARK);
    let provider = RcBlock::new(
        move |appearance: NonNull<NSAppearance>| -> NonNull<NSColor> {
            // SAFETY: AppKit passes a valid appearance for the call's duration.
            let dark_mode = is_dark(unsafe { appearance.as_ref() });
            let color = if dark_mode { &dark } else { &light };
            // The colours live inside the block, which the dynamic colour keeps
            // alive, so the pointer stays valid.
            NonNull::from(&**color)
        },
    );
    // SAFETY: the provider returns a pointer to a colour the block owns.
    unsafe { NSColor::colorWithName_dynamicProvider(None, &provider) }
}

/// Text and symbols drawn on a solid accent fill: black in both modes
/// (white fails WCAG AA on this coral; see the tests).
pub fn on_accent() -> Retained<NSColor> {
    if uses_system_accent() {
        // The system's own pairing for its accent.
        return NSColor::alternateSelectedControlTextColor();
    }
    NSColor::blackColor()
}

/// A stretchable rounded-rectangle mask (for a visual effect view's
/// corners). Only its alpha matters.
pub fn rounded_mask(radius: f64) -> Retained<NSImage> {
    let side = radius * 2.0 + 1.0;
    let draw = RcBlock::new(move |rect: NSRect| -> Bool {
        NSColor::blackColor().setFill();
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, radius, radius).fill();
        Bool::YES
    });
    let image =
        NSImage::imageWithSize_flipped_drawingHandler(NSSize::new(side, side), false, &draw);
    image.setCapInsets(NSEdgeInsets {
        top: radius,
        left: radius,
        bottom: radius,
        right: radius,
    });
    // Stretch the middle, keep the corners.
    image.setResizingMode(NSImageResizingMode(1));
    image
}

/// WCAG 2 relative luminance of an sRGB colour.
pub fn luminance(rgb: u32) -> f64 {
    let lin = |shift: u32| {
        let c = f64::from((rgb >> shift) & 0xFF) / 255.0;
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(16) + 0.7152 * lin(8) + 0.0722 * lin(0)
}

/// WCAG 2 contrast ratio between two sRGB colours.
pub fn contrast(a: u32, b: u32) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLACK: u32 = 0x000000;
    const WHITE: u32 = 0xFFFFFF;
    // Typical macOS backgrounds the accent sits on.
    const LIGHT_BACKGROUNDS: [u32; 2] = [0xFFFFFF, 0xECECEC];
    const DARK_BACKGROUNDS: [u32; 2] = [0x1E1E1E, 0x323232];

    #[test]
    fn text_on_the_accent_passes_aa() {
        // Black text (what `on_accent` uses) needs 4.5:1 in both modes.
        assert!(
            contrast(ACCENT_LIGHT, BLACK) >= 4.5,
            "{}",
            contrast(ACCENT_LIGHT, BLACK)
        );
        assert!(
            contrast(ACCENT_DARK, BLACK) >= 4.5,
            "{}",
            contrast(ACCENT_DARK, BLACK)
        );
        // White would not: this is why `on_accent` is black.
        assert!(contrast(ACCENT_LIGHT, WHITE) < 4.5);
    }

    #[test]
    fn the_accent_stands_out_as_a_ui_colour() {
        // Non-text UI (glyphs, fills, outlines) needs 3:1 against its
        // background (WCAG 1.4.11).
        for bg in LIGHT_BACKGROUNDS {
            assert!(contrast(ACCENT_LIGHT, bg) >= 3.0, "{bg:06x}");
        }
        for bg in DARK_BACKGROUNDS {
            assert!(contrast(ACCENT_DARK, bg) >= 3.0, "{bg:06x}");
        }
    }

    #[test]
    fn contrast_matches_the_wcag_examples() {
        assert!((contrast(BLACK, WHITE) - 21.0).abs() < 1e-9);
        assert!((contrast(ACCENT_LIGHT, BLACK) - 5.42).abs() < 0.01);
        assert!((contrast(ACCENT_DARK, BLACK) - 7.54).abs() < 0.01);
    }
}
