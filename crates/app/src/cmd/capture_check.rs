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
}

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
