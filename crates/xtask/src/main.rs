//! Developer tasks, run as `cargo xtask <task>`.
//!
//! `bundle` (assemble and sign `Glance.app`) is implemented in M2.
//! See `docs/DESIGN.md` §10.

use std::process::ExitCode;

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("bundle") => {
            eprintln!("xtask bundle: not implemented until M2 (see docs/DESIGN.md §10)");
            ExitCode::from(2)
        }
        _ => {
            eprintln!(
                "usage: cargo xtask <task>\n\ntasks:\n  bundle    assemble and sign Glance.app (M2)"
            );
            ExitCode::from(2)
        }
    }
}
