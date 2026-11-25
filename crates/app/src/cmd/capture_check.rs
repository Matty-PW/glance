//! `glance capture-check`: capture the active display a few times and print
//! sizes and timings only. No pixels are written anywhere.

use std::time::Instant;

use anyhow::{Result, bail};
use glance_capture::ScreenCapture;
use glance_macos::capture::LiveScreen;
use glance_macos::permission::{request_screen_recording, screen_recording_granted};

use super::ms;

#[derive(clap::Args)]
pub struct Args {
    /// Longest edge of the image sent to the model, in pixels.
    #[arg(long, default_value_t = 1024)]
    max_edge: u32,
    /// Largest capture taken from the display, in pixels.
    #[arg(long, default_value_t = 3024)]
    capture_edge: u32,
    #[arg(long, default_value_t = 3)]
    repeat: u32,
    /// Instead: prove the screenshot indicator never appears in a capture.
    /// Holds the glow on the active display, captures it as glance does and
    /// once with glance's windows left in, and compares the edges with a
    /// capture taken before the glow. Prints numbers only. Keep the screen
    /// still while it runs (about a second).
    #[arg(long)]
    indicator: bool,
}

/// Outer pixels compared by `--indicator` (the glow's brightest part).
const EDGE_BAND: u32 = 24;

pub fn run(args: Args) -> Result<()> {
    if !screen_recording_granted() {
        // Shows the system prompt the first time; attributed to the terminal
        // when run from one.
        request_screen_recording();
        bail!(
            "Screen Recording permission is not granted. Allow this app (your terminal, when \
             run from one) in System Settings > Privacy & Security > Screen & System Audio \
             Recording, then run again (the app may need a relaunch)"
        );
    }
    let screen = LiveScreen::new(args.capture_edge);
    if args.indicator {
        return indicator_check(&screen);
    }
    println!("run   capture    size          encode     model image   png");
    for run in 1..=args.repeat {
        let started = Instant::now();
        let shot = screen.capture()?;
        let capture = started.elapsed();
        let started = Instant::now();
        let image = shot.encode(args.max_edge)?;
        let encode = started.elapsed();
        println!(
            "{run:<5} {:>6.1} ms  {:>5}x{:<5}  {:>6.1} ms  {:>5}x{:<5}  {} kB",
            ms(capture),
            shot.width(),
            shot.height(),
            ms(encode),
            image.width(),
            image.height(),
            image.len() / 1000
        );
    }
    Ok(())
}

fn indicator_check(screen: &LiveScreen) -> Result<()> {
    use glance_macos::glow::{self, Verdict};
    let check = glow::exclusion_check(screen, EDGE_BAND).map_err(anyhow::Error::msg)?;
    println!(
        "edge difference from a capture without the glow (outer {EDGE_BAND} px, mean per channel, 0-255):"
    );
    println!(
        "  glance's capture (its windows excluded): {:>6.2}   must be under {}",
        check.excluded,
        glow::UNCHANGED
    );
    println!(
        "  control (glance's windows left in):      {:>6.2}   must be over {}",
        check.control,
        glow::SEEN
    );
    match check.verdict() {
        Verdict::Pass => {
            println!("PASS: the glow was on screen and is not in glance's capture");
        }
        Verdict::Fail => bail!(
            "FAIL: glance's capture changed at the edges while the glow was up (if the screen \
             was changing, run again with it still)"
        ),
        Verdict::Inconclusive => bail!(
            "INCONCLUSIVE: the control did not see the glow either, so this run proves nothing \
             (try again with the display showing something other than the accent colour)"
        ),
    }

    // Then one real, animated flash: it must show and then leave the screen.
    let flash = glow::flash_check(screen, EDGE_BAND).map_err(anyhow::Error::msg)?;
    println!("one animated flash (control captures, glance's windows left in):");
    println!(
        "  0.12 s in:  {:>6.2}   must be over {}",
        flash.during,
        glow::SEEN
    );
    println!(
        "  1.1 s in:   {:>6.2}   must be under {}",
        flash.after,
        glow::UNCHANGED
    );
    if !flash.played_and_left() {
        bail!("FAIL: the flash did not show, or did not leave the screen");
    }
    println!("PASS: the flash showed and then left the screen");
    Ok(())
}
