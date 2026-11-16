//! `glance mic-check`: record briefly and print level and timing statistics.
//! Nothing is transcribed, stored, or printed beyond the numbers below.

use std::time::{Duration, Instant};

use anyhow::Result;
use glance_audio::{AudioInput, Microphone, resample};

use super::ms;

#[derive(clap::Args)]
pub struct Args {
    #[arg(long, default_value_t = 2.0)]
    seconds: f32,
}

pub fn run(args: Args) -> Result<()> {
    let mut mic = Microphone::new(60);
    let started = Instant::now();
    mic.start()?;
    let opened = started.elapsed();
    std::thread::sleep(Duration::from_secs_f32(args.seconds));
    let recording = mic.stop()?;
    let clip = recording.clip;

    let samples = clip.samples();
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt();
    println!("device rate        {} Hz", clip.sample_rate());
    println!(
        "recorded           {:.2} s of {:.2} s requested",
        clip.duration().as_secs_f64(),
        args.seconds
    );
    println!("stream open        {:.1} ms", ms(opened));
    match recording.start_latency {
        Some(latency) => println!("first samples at   {:.1} ms after start", ms(latency)),
        None => println!("first samples at   never (no audio callback ran)"),
    }
    println!("level              rms {rms:.4}, peak {peak:.4}");
    if clip.is_silent() {
        println!(
            "result             SILENT: grant Microphone access to this terminal in System Settings > Privacy & Security"
        );
    } else {
        let started = Instant::now();
        let resampled = resample(&clip, 16_000)?;
        println!(
            "resample to 16 kHz {:.1} ms ({} samples)",
            ms(started.elapsed()),
            resampled.samples().len()
        );
        println!("result             OK");
    }
    Ok(())
}
