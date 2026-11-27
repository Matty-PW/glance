//! Screen Recording and Microphone permissions (`docs/DESIGN.md` §10).
//!
//! Screen Recording: CoreGraphics preflight (no prompt) and request (the
//! prompt, shown once). Microphone: status only, from AVFoundation. macOS
//! asks by itself the first time a microphone stream opens, which in glance
//! is the first chord press; until access is granted the device delivers
//! silence, which `glance_audio::AudioClip::is_silent` detects.
//!
//! Setup can also ask for the microphone up front
//! ([`request_microphone`]), and tell a Screen Recording grant that needs a
//! restart from none at all ([`screen_recording_fresh`]).
//!
//! `unsafe` here: reading an AVFoundation constant and calling two class
//! methods through the generated bindings.
#![allow(unsafe_code)]

use std::time::{Duration, Instant};

use block2::RcBlock;
use glance_orchestrator::setup::MicStatus;
use objc2::runtime::Bool;
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
use objc2_core_graphics::{CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess};

/// System Settings, Privacy & Security, Screen Recording.
pub const SCREEN_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";
/// System Settings, Privacy & Security, Microphone.
pub const MIC_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone";

/// Open a System Settings pane. `open` with one of the fixed URLs above;
/// nothing from the screen or a model ever reaches this.
pub fn open_settings_pane(url: &'static str) {
    debug_assert!(url == SCREEN_SETTINGS || url == MIC_SETTINGS);
    if let Err(e) = std::process::Command::new("/usr/bin/open").arg(url).spawn() {
        eprintln!("glance: could not open System Settings: {e}");
    }
}

/// The hidden subcommand [`screen_recording_fresh`] runs.
pub const PROBE_ARG: &str = "permission-probe";

/// Screen Recording as a new process sees it now. A child of glance has the
/// same identity for macOS's privacy checks, but reads the current grant,
/// which a running process may only see after a restart. `None` when the
/// probe could not run or did not answer within 2 s.
pub fn screen_recording_fresh() -> Option<bool> {
    let exe = std::env::current_exe().ok()?;
    let mut child = std::process::Command::new(exe)
        .arg(PROBE_ARG)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        if let Ok(Some(status)) = child.try_wait() {
            return match status.code() {
                Some(0) => Some(true),
                Some(1) => Some(false),
                _ => None,
            };
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

/// What `glance permission-probe` exits with: 0 granted, 1 not.
pub fn probe_exit_code() -> i32 {
    if screen_recording_granted() { 0 } else { 1 }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicPermission {
    /// Not asked yet; the prompt appears on the first recording.
    NotAsked,
    Granted,
    Denied,
    /// Blocked by a device policy (MDM, parental controls).
    Restricted,
    /// AVFoundation did not answer in a known way.
    Unknown,
}

/// Current microphone permission of this process (or of the app that is
/// responsible for it, such as the terminal for a CLI run).
pub fn microphone_permission() -> MicPermission {
    // SAFETY: `AVMediaTypeAudio` is an immutable constant exported by
    // AVFoundation, which the bindings link.
    let Some(audio) = (unsafe { AVMediaTypeAudio }) else {
        return MicPermission::Unknown;
    };
    // SAFETY: the method accepts exactly AVMediaTypeVideo or AVMediaTypeAudio
    // (anything else raises an exception); this passes AVMediaTypeAudio.
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(audio) };
    match status {
        AVAuthorizationStatus::NotDetermined => MicPermission::NotAsked,
        AVAuthorizationStatus::Authorized => MicPermission::Granted,
        AVAuthorizationStatus::Denied => MicPermission::Denied,
        AVAuthorizationStatus::Restricted => MicPermission::Restricted,
        _ => MicPermission::Unknown,
    }
}

/// The microphone status for setup's classification.
pub fn mic_status() -> MicStatus {
    match microphone_permission() {
        MicPermission::NotAsked => MicStatus::NotDetermined,
        MicPermission::Granted => MicStatus::Authorized,
        MicPermission::Denied => MicStatus::Denied,
        MicPermission::Restricted => MicStatus::Restricted,
        MicPermission::Unknown => MicStatus::Unknown,
    }
}

/// Show macOS's microphone prompt (only while not yet asked; otherwise it
/// answers at once). `done(granted)` runs on an arbitrary queue.
pub fn request_microphone(done: impl Fn(bool) + Send + 'static) {
    // SAFETY: as in `microphone_permission`.
    let Some(audio) = (unsafe { AVMediaTypeAudio }) else {
        done(false);
        return;
    };
    let handler = RcBlock::new(move |granted: Bool| done(granted.as_bool()));
    // SAFETY: AVMediaTypeAudio is one of the two accepted media types; the
    // block is retained by AVFoundation until it has run.
    unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(audio, &handler) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whatever the machine's state, the call must answer without crashing
    /// and without prompting.
    #[test]
    fn status_can_be_read() {
        let status = microphone_permission();
        assert_ne!(status, MicPermission::Unknown);
    }
}
