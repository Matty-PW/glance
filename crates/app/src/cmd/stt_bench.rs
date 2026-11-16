//! `glance stt-bench`: time a speech model on WAV clips and, when a reference
//! transcript sits next to a clip (`clip.wav` + `clip.txt`), score word errors.
//!
//! For benchmarking on synthetic or public clips only.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use glance_stt::{Parakeet, SpeechToText};

use super::{find_model, manifest, ms, store};

#[derive(clap::Args)]
pub struct Args {
    /// Speech model id from models.toml.
    #[arg(long)]
    stt: String,
    /// 16 kHz mono 16-bit WAV files.
    #[arg(required = true)]
    wav: Vec<PathBuf>,
    /// Timed runs per clip (after one untimed warm-up); the median is reported.
    #[arg(long, default_value_t = 5)]
    runs: usize,
}

pub fn run(args: Args) -> Result<()> {
    let manifest = manifest()?;
    let model = find_model(&manifest, &args.stt)?;
    let started = Instant::now();
    let dir = store()?
        .verified_dir(model)
        .with_context(|| format!("run `glance models pull {}` first", model.id))?;
    let verify = started.elapsed();
    let started = Instant::now();
    let mut stt = Parakeet::load(&dir)?;
    println!(
        "{}: verified in {:.0} ms, loaded in {:.0} ms",
        model.id,
        ms(verify),
        ms(started.elapsed())
    );

    let (mut errors, mut words, mut audio_s, mut compute_s) = (0usize, 0usize, 0.0f64, 0.0f64);
    println!(
        "{:<14} {:>7} {:>9} {:>7} {:>6}  transcript",
        "clip", "audio s", "median ms", "x real", "WER"
    );
    for path in &args.wav {
        let clip = glance_stt::read_wav(path)?;
        let seconds = clip.duration().as_secs_f64();
        let mut text = stt.transcribe(&clip)?.text; // warm-up
        let mut times = Vec::with_capacity(args.runs);
        for _ in 0..args.runs.max(1) {
            let started = Instant::now();
            text = stt.transcribe(&clip)?.text;
            times.push(started.elapsed());
        }
        times.sort();
        let median = times[times.len() / 2];

        let reference = std::fs::read_to_string(path.with_extension("txt")).ok();
        let wer = reference.as_deref().map(|reference| {
            let (e, n) = word_errors(reference, &text);
            errors += e;
            words += n;
            format!("{:.0}%", 100.0 * e as f64 / n.max(1) as f64)
        });
        audio_s += seconds;
        compute_s += median.as_secs_f64();
        println!(
            "{:<14} {:>7.2} {:>9.0} {:>7.1} {:>6}  {}",
            path.file_stem().and_then(|s| s.to_str()).unwrap_or("?"),
            seconds,
            ms(median),
            seconds / median.as_secs_f64(),
            wer.as_deref().unwrap_or("-"),
            text
        );
    }
    println!(
        "total: {:.1} s audio in {:.0} ms ({:.1}x real time)",
        audio_s,
        compute_s * 1000.0,
        audio_s / compute_s
    );
    if words > 0 {
        println!(
            "word error rate: {:.1}% ({errors} errors / {words} reference words)",
            100.0 * errors as f64 / words as f64
        );
    }
    Ok(())
}

/// Lowercase, strip punctuation, split into words.
fn normalize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Word-level edit distance between `hypothesis` and `reference`, and the
/// number of reference words.
fn word_errors(reference: &str, hypothesis: &str) -> (usize, usize) {
    let (r, h) = (normalize(reference), normalize(hypothesis));
    let mut row: Vec<usize> = (0..=h.len()).collect();
    for (i, rw) in r.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, hw) in h.iter().enumerate() {
            let substitution = diagonal + usize::from(rw != hw);
            diagonal = row[j + 1];
            row[j + 1] = substitution.min(row[j] + 1).min(diagonal + 1);
        }
    }
    (row[h.len()], r.len())
}

#[cfg(test)]
mod tests {
    use super::word_errors;

    #[test]
    fn word_error_counting() {
        assert_eq!(
            word_errors("What is on my screen?", "what is on my screen"),
            (0, 5)
        );
        assert_eq!(
            word_errors("what is on my screen", "what is in my screen"),
            (1, 5)
        );
        assert_eq!(
            word_errors("what is on my screen", "what is my screen"),
            (1, 5)
        );
        assert_eq!(
            word_errors("what is on my screen", "so what is on my screen"),
            (1, 5)
        );
        assert_eq!(word_errors("a b c", ""), (3, 3));
    }
}
