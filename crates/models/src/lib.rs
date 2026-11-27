//! Model manifest (`models.toml`) parsing, download, and verification.
//!
//! The only crate that downloads anything, and one of the two allowed to depend
//! on an HTTP client (enforced in `deny.toml`). Nothing is loaded unless it
//! matches its pinned size and SHA-256. See `docs/DESIGN.md` §8.

pub mod download;
pub mod manifest;
pub mod store;

pub use download::{DownloadError, Failure, Outcome, pull};
pub use manifest::{Format, Kind, Manifest, ManifestError, Model, ModelFile, Role};
pub use store::{ModelStore, Partial, VerifyError};
