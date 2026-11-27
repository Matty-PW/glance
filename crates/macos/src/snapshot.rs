//! `glance ui-snapshots`: draw glance's own windows (overlay states, the
//! screenshot indicator's looks and settings pages, light and dark, with
//! fixed demo text) into PNG files,
//! to review the layout. It renders views in-process; it never captures the
//! screen, and nothing from other apps can appear in the images. Visual
//! effect views (blur) are not drawn this way, so the overlay is shown with
//! its solid (Reduce Transparency) background.
#![allow(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use glance_orchestrator::settings::{ModelsBackend, SettingsBackend};
use glance_orchestrator::setup::{Access, SetupBackend, Step};
use glance_orchestrator::{Progress, ScreenshotUse, TurnState};
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
    let data = png_of(view)?;
    std::fs::write(path, data).map_err(|e| format!("{}: {e}", path.display()))
}

/// Draw `view` (at its size, at the screen's scale) into PNG bytes.
pub(crate) fn png_of(view: &NSView) -> Result<Vec<u8>, String> {
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
    Ok(data.to_vec())
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
    // A new screenshot, or (a follow-up, think harder) one from 42 s ago.
    let new = Some(ScreenshotUse::Fresh { display: None });
    let reused = Some(ScreenshotUse::Reused {
        taken: std::time::Instant::now() - std::time::Duration::from_secs(42),
    });
    type State<'a> = (
        &'a str,
        TurnState,
        Option<Progress>,
        &'a str,
        &'a str,
        Option<ScreenshotUse>,
    );
    let states: [State; 8] = [
        ("ready", TurnState::Idle, None, QUESTION, SHORT, new),
        ("listening", TurnState::Listening, None, "", "", new),
        ("transcribing", TurnState::Transcribing, None, "", "", new),
        ("thinking", TurnState::Thinking, None, QUESTION, "", new),
        (
            "thinking-harder",
            TurnState::Thinking,
            Some(Progress::ThinkingHarder),
            QUESTION,
            "",
            reused,
        ),
        ("speaking", TurnState::Speaking, None, QUESTION, SHORT, new),
        (
            "speaking-reused-shot",
            TurnState::Speaking,
            None,
            QUESTION,
            SHORT,
            reused,
        ),
        (
            "speaking-long",
            TurnState::Speaking,
            None,
            QUESTION,
            &long,
            new,
        ),
    ];
    let mut written = Vec::new();
    for dark in [false, true] {
        for (name, state, progress, question, reply, shot) in states {
            let levels = [0.01, 0.04, 0.08, 0.05, 0.02];
            let (_panel, view) =
                crate::overlay::stage(state, progress, question, reply, shot, levels, dark)
                    .ok_or("overlay not installed")?;
            let mode = if dark { "dark" } else { "light" };
            let path = out.join(format!("overlay-{name}-{mode}.png"));
            save_view(&view, &path)?;
            written.push(path);
        }
    }
    Ok(written)
}

/// Render each screenshot-indicator look at full strength, light and dark,
/// on a plain backdrop, into `out`. Main thread.
pub fn glow(out: &Path) -> Result<Vec<PathBuf>, String> {
    let mtm = MainThreadMarker::new().ok_or("must run on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let size = objc2_foundation::NSSize::new(900.0, 560.0);
    let mut written = Vec::new();
    for dark in [false, true] {
        for (caption, look) in crate::glow::demo_looks() {
            let view = crate::glow::stage(look, size, dark).ok_or("no main thread")?;
            let mode = if dark { "dark" } else { "light" };
            let name = caption.to_lowercase().replace(' ', "-");
            let path = out.join(format!("glow-{name}-{mode}.png"));
            save_view(&view, &path)?;
            written.push(path);
        }
    }
    Ok(written)
}

/// Render every settings section in light and dark into `out`, using the
/// given (demo) backends. Main thread.
pub fn settings(
    out: &Path,
    settings: Arc<dyn SettingsBackend>,
    models: Arc<dyn ModelsBackend>,
) -> Result<Vec<PathBuf>, String> {
    let mtm = MainThreadMarker::new().ok_or("must run on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    crate::shell::install_backends(settings, models);
    let mut written = Vec::new();
    let count = crate::settings::stage(0, false)
        .ok_or("no settings window")?
        .2;
    let names = crate::settings::section_names();
    for dark in [false, true] {
        for (i, name) in names.iter().enumerate().take(count) {
            let (_window, view, _) = crate::settings::stage(i, dark).ok_or("no settings window")?;
            let mode = if dark { "dark" } else { "light" };
            let file = name.to_lowercase().replace(' ', "-");
            let path = out.join(format!("settings-{file}-{mode}.png"));
            save_view(&view, &path)?;
            written.push(path);
        }
    }
    // The worst case for clipping: the smallest window, longest names.
    crate::settings::set_minimum_size(true);
    for (i, name) in names.iter().enumerate().take(count) {
        let (_window, view, _) = crate::settings::stage(i, false).ok_or("no settings window")?;
        let file = name.to_lowercase().replace(' ', "-");
        let path = out.join(format!("settings-{file}-min-size.png"));
        save_view(&view, &path)?;
        written.push(path);
    }
    crate::settings::set_minimum_size(false);
    Ok(written)
}

/// One picture of the setup window: the backend is put into the scene
/// called `name` first (demo data), the permission states are fixed.
pub struct SetupScene {
    pub name: &'static str,
    pub step: Step,
    pub screen: Access,
    pub mic: Access,
    pub signing: crate::signing::Signing,
    /// Show Your Mac's "Advanced" part open.
    pub advanced: bool,
}

/// Render each scene of the setup window in light and dark (and the
/// smallest window in light) into `out`. Main thread.
pub fn setup(
    out: &Path,
    backend: Arc<dyn SetupBackend>,
    scenes: &[SetupScene],
    prepare: &dyn Fn(&str),
) -> Result<Vec<PathBuf>, String> {
    let mtm = MainThreadMarker::new().ok_or("must run on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    crate::shell::install_setup(backend);
    let mut written = Vec::new();
    for (mode, dark, small) in [
        ("light", false, false),
        ("dark", true, false),
        ("min-size", false, true),
    ] {
        for scene in scenes {
            prepare(scene.name);
            let perms = Box::new(crate::setup::perms::Demo::fixed(
                scene.screen,
                scene.mic,
                scene.signing,
            ));
            let (_window, view) = crate::setup::stage(scene.step, dark, perms, scene.advanced)
                .ok_or("no setup window")?;
            crate::setup::set_minimum_size(small);
            let path = out.join(format!("setup-{}-{mode}.png", scene.name));
            save_view(&view, &path)?;
            written.push(path);
        }
    }
    crate::setup::set_minimum_size(false);
    Ok(written)
}

/// The setup test's sample chart as `glance` renders it in memory, saved
/// here only to review it.
pub fn sample_chart(out: &Path) -> Result<PathBuf, String> {
    let bytes = crate::sample::chart_png()?;
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let path = out.join("setup-sample-chart.png");
    std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}
