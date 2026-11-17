//! Microphone permission status, read from AVFoundation.
//!
//! This never shows a prompt. macOS asks by itself the first time a
//! microphone stream opens, which in glance is the first chord press
//! (`docs/DESIGN.md` §10). Until access is granted the device delivers
//! silence, which [`crate::AudioClip::is_silent`] detects.
//!
//! The only `unsafe` in this crate: reading an AVFoundation constant and
//! calling one class method through the generated bindings.
#![allow(unsafe_code)]

use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};

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
