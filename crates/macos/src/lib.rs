//! glance on macOS. Everything platform-specific lives here, behind the
//! core crates' traits; no AppKit, ScreenCaptureKit, AVFoundation or Carbon
//! type reaches them:
//!
//! - [`capture::LiveScreen`]: `ScreenCapture` with ScreenCaptureKit.
//! - [`system_voice::SystemVoice`]: `TextToSpeech` with AVSpeechSynthesizer.
//! - [`permission`]: Screen Recording and Microphone permission state.
//! - [`hotkeys`]: the chords as Carbon hotkeys (no Input Monitoring).
//! - [`shell`]: the app (NSApplication run loop, menu-bar icon, overlay,
//!   settings window), the orchestrator's `Shell`.
//! - [`theme`]: the accent colour and spacing, the only colour values here.
//! - [`snapshot`]: `glance ui-snapshots`, glance's own windows drawn to PNGs.
//! - [`bundle`]: assembling and signing `Glance.app` (used by `cargo xtask`).
//!
//! On any other platform this crate is empty.
#![cfg(target_os = "macos")]

pub mod bundle;
pub mod capture;
pub mod hotkeys;
mod overlay;
pub mod permission;
mod settings;
pub mod shell;
pub mod snapshot;
pub mod system_voice;
pub mod theme;
mod tray;
