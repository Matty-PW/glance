//! The `glance` binary: main-thread event loop, hotkey and tray glue, and the CLI.
//!
//! Nothing is wired up yet; this is the Step 2 skeleton. See `docs/DESIGN.md` §14
//! for what each milestone adds.

fn main() {
    println!(
        "glance {} (skeleton only: see docs/DESIGN.md)",
        env!("CARGO_PKG_VERSION")
    );
}
