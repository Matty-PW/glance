//! Developer tasks, run as `cargo xtask <task>`.
//!
//! `bundle`: build and sign `target/bundle/Glance.app`. The work is in
//! `glance_macos::bundle`, with the rest of the macOS-specific code.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let result = match std::env::args().nth(1).as_deref() {
        #[cfg(target_os = "macos")]
        Some("bundle") => glance_macos::bundle::bundle(&workspace_root()),
        _ => {
            eprintln!(
                "usage: cargo xtask <task>\n\ntasks:\n  bundle    build and sign target/bundle/Glance.app (macOS)"
            );
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("xtask: error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn workspace_root() -> PathBuf {
    // crates/xtask -> workspace root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("xtask lives two levels below the workspace root")
        .to_path_buf()
}
