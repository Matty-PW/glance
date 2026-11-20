//! `glance ui-snapshots`: glance's own windows drawn to PNGs (overlay states
//! and settings pages, light and dark, fixed demo text), to review layout.
//! Development only. It never captures the screen.

use std::path::PathBuf;

use anyhow::Result;

#[derive(clap::Args)]
pub struct Args {
    /// Where the PNGs go (keep it in the gitignored eval-local/).
    #[arg(long, default_value = "eval-local/ui")]
    out: PathBuf,
}

pub fn run(args: Args) -> Result<()> {
    eprintln!(
        "glance: drawing glance's own windows with demo text into {} (no screen capture)",
        args.out.display()
    );
    let written = glance_macos::snapshot::overlay(&args.out).map_err(anyhow::Error::msg)?;
    for path in written {
        println!("{}", path.display());
    }
    Ok(())
}
