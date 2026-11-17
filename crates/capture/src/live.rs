//! Live capture of the active display through ScreenCaptureKit.
//!
//! "Active display" is the display holding the frontmost app's largest
//! normal window, falling back to the main display. glance's own windows are
//! excluded from the image, so the overlay (M5) never appears in a capture.
//! The pixels stay in memory.

use objc2_app_kit::NSWorkspace;
use objc2_core_graphics::{
    CGMainDisplayID, CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess,
};
use screencapturekit::prelude::{
    CGRect, SCContentFilter, SCShareableContent, SCStreamConfiguration,
};
use screencapturekit::screenshot_manager::{CGImageExt, SCScreenshotManager};

use crate::{CaptureError, ScreenCapture, Screenshot, fit_within};

/// Captures the active display at up to `max_edge` pixels on the long side.
#[derive(Debug, Clone, Copy)]
pub struct LiveScreen {
    max_edge: u32,
}

impl LiveScreen {
    /// `max_edge` caps the captured size; models get a further downscaled
    /// copy (see [`Screenshot::encode`]).
    pub fn new(max_edge: u32) -> Self {
        Self { max_edge }
    }
}

/// Whether this process may capture the screen. Never shows a prompt.
pub fn screen_recording_granted() -> bool {
    CGPreflightScreenCaptureAccess()
}

/// Ask for Screen Recording permission. macOS shows its prompt only the
/// first time; after that this just reports the current state. A grant
/// usually takes effect only after the app is relaunched.
pub fn request_screen_recording() -> bool {
    CGRequestScreenCaptureAccess()
}

impl ScreenCapture for LiveScreen {
    fn capture(&self) -> Result<Screenshot, CaptureError> {
        if !screen_recording_granted() {
            return Err(CaptureError::PermissionDenied);
        }
        let failed = |e: &dyn std::fmt::Debug| CaptureError::Capture(format!("{e:?}"));
        let content = SCShareableContent::create()
            .with_on_screen_windows_only(true)
            .with_exclude_desktop_windows(true)
            .get()
            .map_err(|e| failed(&e))?;

        let displays = content.displays();
        let windows = content.windows();
        let front_pid = NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .map(|app| app.processIdentifier());
        let display_frames: Vec<(u32, Rect)> = displays
            .iter()
            .map(|d| (d.display_id(), Rect::from(d.frame())))
            .collect();
        let window_info: Vec<WindowInfo> = windows
            .iter()
            .map(|w| WindowInfo {
                pid: w.owning_application().map(|a| a.process_id()),
                layer: w.window_layer(),
                frame: Rect::from(w.frame()),
            })
            .collect();
        let index = pick_display(&display_frames, &window_info, front_pid, CGMainDisplayID())
            .ok_or(CaptureError::NoDisplay)?;
        let display = &displays[index];

        // Leave glance's own windows out of the picture.
        let own_pid = i32::try_from(std::process::id()).unwrap_or(-1);
        let applications = content.applications();
        let own: Vec<_> = applications
            .iter()
            .filter(|a| a.process_id() == own_pid)
            .collect();
        let filter = SCContentFilter::create()
            .with_display(display)
            .with_excluding_applications(&own, &[])
            .build()
            .map_err(|e| failed(&e))?;

        // Retina displays have two pixels per point; never capture more than
        // the cap, and never upscale.
        let frame = display_frames[index].1;
        let (width, height) = fit_within(
            (frame.w * 2.0).round() as u32,
            (frame.h * 2.0).round() as u32,
            self.max_edge,
        );
        let config = SCStreamConfiguration::new()
            .with_width(width)
            .with_height(height);
        let image = SCScreenshotManager::capture_image(&filter, &config).map_err(|e| failed(&e))?;
        let (w, h) = (image.width() as u32, image.height() as u32);
        let pixels = image.rgba_data().map_err(|e| failed(&e))?;
        Screenshot::from_rgba(w, h, pixels)
    }
}

/// A rectangle in global screen points (origin top-left).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl From<CGRect> for Rect {
    fn from(r: CGRect) -> Self {
        Self {
            x: r.origin.x,
            y: r.origin.y,
            w: r.size.width,
            h: r.size.height,
        }
    }
}

impl Rect {
    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

#[derive(Debug, Clone, Copy)]
struct WindowInfo {
    pid: Option<i32>,
    layer: i32,
    frame: Rect,
}

/// Index of the display to capture: the one holding the centre of the
/// frontmost app's largest normal (layer 0) window, else the main display,
/// else the first display.
fn pick_display(
    displays: &[(u32, Rect)],
    windows: &[WindowInfo],
    front_pid: Option<i32>,
    main_display: u32,
) -> Option<usize> {
    let front_window = front_pid.and_then(|pid| {
        windows
            .iter()
            .filter(|w| w.pid == Some(pid) && w.layer == 0 && w.frame.w > 1.0 && w.frame.h > 1.0)
            .max_by(|a, b| (a.frame.w * a.frame.h).total_cmp(&(b.frame.w * b.frame.h)))
    });
    if let Some(window) = front_window {
        let (cx, cy) = (
            window.frame.x + window.frame.w / 2.0,
            window.frame.y + window.frame.h / 2.0,
        );
        if let Some(i) = displays.iter().position(|(_, d)| d.contains(cx, cy)) {
            return Some(i);
        }
    }
    displays
        .iter()
        .position(|(id, _)| *id == main_display)
        .or(if displays.is_empty() { None } else { Some(0) })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEFT: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 1512.0,
        h: 982.0,
    };
    const RIGHT: Rect = Rect {
        x: 1512.0,
        y: -200.0,
        w: 2560.0,
        h: 1440.0,
    };

    fn win(pid: i32, layer: i32, x: f64, y: f64, w: f64, h: f64) -> WindowInfo {
        WindowInfo {
            pid: Some(pid),
            layer,
            frame: Rect { x, y, w, h },
        }
    }

    #[test]
    fn follows_the_frontmost_apps_largest_window() {
        let displays = [(1, LEFT), (2, RIGHT)];
        let windows = [
            win(10, 0, 100.0, 100.0, 300.0, 200.0), // small window on the left
            win(10, 0, 1700.0, 100.0, 1800.0, 1000.0), // big window on the right
            win(99, 0, 0.0, 0.0, 1512.0, 982.0),    // another app
        ];
        assert_eq!(pick_display(&displays, &windows, Some(10), 1), Some(1));
        assert_eq!(pick_display(&displays, &windows, Some(99), 2), Some(0));
    }

    #[test]
    fn ignores_menus_panels_and_tiny_windows() {
        let displays = [(1, LEFT), (2, RIGHT)];
        let windows = [
            win(10, 25, 1700.0, 0.0, 400.0, 400.0), // a menu or panel (layer 25)
            win(10, 0, 1800.0, 0.0, 1.0, 1.0),      // a 1x1 helper window
        ];
        assert_eq!(pick_display(&displays, &windows, Some(10), 1), Some(0));
    }

    #[test]
    fn falls_back_to_the_main_display_then_the_first() {
        let displays = [(1, LEFT), (2, RIGHT)];
        assert_eq!(pick_display(&displays, &[], None, 2), Some(1));
        assert_eq!(pick_display(&displays, &[], Some(10), 77), Some(0));
        assert_eq!(pick_display(&[], &[], None, 1), None);
    }
}
