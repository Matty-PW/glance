//! `glance ui-snapshots`: draw glance's own windows (overlay states and
//! settings pages, light and dark, with fixed demo text) into PNG files,
//! to review the layout. It renders views in-process; it never captures the
//! screen, and nothing from other apps can appear in the images. Visual
//! effect views (blur) are not drawn this way, so the overlay is shown with
//! its solid (Reduce Transparency) background.
#![allow(unsafe_code)]

use std::path::{Path, PathBuf};

use glance_orchestrator::{Progress, TurnState};
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication,
    NSApplicationActivationPolicy, NSBitmapImageFileType, NSView,
};
use objc2_foundation::NSDictionary;

/// Aqua or Dark Aqua.
pub(crate) fn appearance(dark: bool) -> Option<Retained<NSAppearance>> {
    // SAFETY: AppKit's appearance-name constants, valid for the process.
    let name = unsafe {
        if dark {
            NSAppearanceNameDarkAqua
        } else {
            NSAppearanceNameAqua
        }
    };
    NSAppearance::appearanceNamed(name)
}

/// Draw `view` (at its size, at the screen's scale) into a PNG at `path`.
pub(crate) fn save_view(view: &NSView, path: &Path) -> Result<(), String> {
    view.layoutSubtreeIfNeeded();
    let bounds = view.bounds();
    let rep = view
        .bitmapImageRepForCachingDisplayInRect(bounds)
        .ok_or("could not make a bitmap for the view")?;
    view.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
    // SAFETY: an empty properties dictionary of the declared type.
    let data = unsafe {
        rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }
    .ok_or("could not encode the PNG")?;
    std::fs::write(path, data.to_vec()).map_err(|e| format!("{}: {e}", path.display()))
}

const QUESTION: &str = "What does this chart show?";
const SHORT: &str = "Weekly active users rose from 1,240 to 1,720 over four weeks.";

/// Render every overlay state in light and dark into `out`. Main thread.
pub fn overlay(out: &Path) -> Result<Vec<PathBuf>, String> {
    let mtm = MainThreadMarker::new().ok_or("must run on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    crate::overlay::install();
    let long = [
        "Week three is the only week where sign-ups fell, from 410 to 365.",
        "The note under the chart says the onboarding email was paused that week, which matches the dip.",
        "Returning users held steady through the whole month, so the dip comes from fewer new users rather than lost ones.",
        "When the email came back in week four, sign-ups jumped to 520, the best week on the chart.",
    ]
    .join(" ")
    .repeat(3);
    let states: [(&str, TurnState, Option<Progress>, &str, &str); 6] = [
        ("listening", TurnState::Listening, None, "", ""),
        ("transcribing", TurnState::Transcribing, None, "", ""),
        ("thinking", TurnState::Thinking, None, QUESTION, ""),
        (
            "thinking-harder",
            TurnState::Thinking,
            Some(Progress::ThinkingHarder),
            QUESTION,
            "",
        ),
        ("speaking", TurnState::Speaking, None, QUESTION, SHORT),
        ("speaking-long", TurnState::Speaking, None, QUESTION, &long),
    ];
    let mut written = Vec::new();
    for dark in [false, true] {
        for (name, state, progress, question, reply) in states {
            let levels = [0.01, 0.04, 0.08, 0.05, 0.02];
            let (_panel, view) =
                crate::overlay::stage(state, progress, question, reply, levels, dark)
                    .ok_or("overlay not installed")?;
            let mode = if dark { "dark" } else { "light" };
            let path = out.join(format!("overlay-{name}-{mode}.png"));
            save_view(&view, &path)?;
            written.push(path);
        }
    }
    Ok(written)
}
