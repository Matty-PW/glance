//! Screen capture behind the `ScreenCapture` trait.
//!
//! The ScreenCaptureKit implementation lands in M2. Screenshots are held in an
//! opaque in-memory type and are never written to disk or logs.
//! See `docs/DESIGN.md` §2–4.
