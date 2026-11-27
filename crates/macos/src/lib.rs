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
//! - `setup`: the first-launch setup window (shown through [`shell`]).
//! - [`glow`]: the screenshot indicator, a glow around the captured display.
//! - [`theme`]: the accent colour and spacing, the only colour values here.
//! - [`snapshot`]: `glance ui-snapshots`, glance's own windows drawn to PNGs.
//! - [`signing`]: whether this build is signed ad hoc (permissions lost at
//!   each rebuild).
//! - [`sample`]: the setup test's built-in chart, drawn in memory.
//! - [`bundle`]: assembling and signing `Glance.app` (used by `cargo xtask`).
//!
//! On any other platform this crate is empty.
#![cfg(target_os = "macos")]

pub mod bundle;
pub mod capture;
pub mod glow;
pub mod hotkeys;
mod overlay;
pub mod permission;
pub mod sample;
mod settings;
mod setup;
pub mod shell;
pub mod signing;
pub mod snapshot;
pub mod system_voice;
pub mod theme;
mod tray;
