//! `glance models …`: the only way weights reach the machine.

use std::io::Write;
use std::time::Instant;

use anyhow::{Result, bail};
use clap::Subcommand;
use glance_models::Outcome;

use super::{find_model, manifest, store};

#[derive(Subcommand)]
pub enum Action {
    /// Show every pinned model and whether it is downloaded and verified.
    List,
    /// Download a model from Hugging Face and verify it against its pins.
    Pull { id: String },
    /// Re-hash downloaded files against their pins (all models, or one).
    Verify { id: Option<String> },
}

pub async fn run(action: Action) -> Result<()> {
    let manifest = manifest()?;
    let store = store()?;
    match action {
        Action::List => {
            println!("store: {}", store.root().display());
            for model in manifest.models() {
                let bytes: u64 = model.files.iter().map(|f| f.size).sum();
                let present = model
                    .files
                    .iter()
                    .filter(|f| store.file_path(model, f).is_file())
                    .count();
                println!(
                    "{:<34} {:?}  {:>6.2} GB  {}/{} files present  [{}]",
                    model.id,
                    model.role,
                    bytes as f64 / 1e9,
                    present,
                    model.files.len(),
                    model.license
                );
            }
            Ok(())
        }
        Action::Pull { id } => {
            let model = find_model(&manifest, &id)?;
            eprintln!(
                "Downloading `{}` from huggingface.co/{} at commit {} ({})",
                model.id, model.repo, model.revision, model.license
            );
            let mut last_percent = u64::MAX;
            let outcomes = glance_models::pull(&store, model, |file, done, total| {
                let percent = done * 100 / total.max(1);
                if percent != last_percent {
                    last_percent = percent;
                    eprint!("\r  {:<44} {:>3}%", file.path, percent);
                    let _ = std::io::stderr().flush();
                }
            })
            .await?;
            eprintln!();
            for (path, outcome) in outcomes {
                let note = match outcome {
                    Outcome::AlreadyVerified => "already present, SHA-256 verified",
                    Outcome::Downloaded => "downloaded, SHA-256 verified",
                };
                println!("  {path}: {note}");
            }
            Ok(())
        }
        Action::Verify { id } => {
            let models: Vec<_> = match &id {
                Some(id) => vec![find_model(&manifest, id)?],
                None => manifest.models().iter().collect(),
            };
            let mut failures = 0;
            for model in models {
                for file in &model.files {
                    let started = Instant::now();
                    match store.verify_file(model, file) {
                        Ok(_) => println!(
                            "ok    {}/{}  ({:.1}s)",
                            model.id,
                            file.path,
                            started.elapsed().as_secs_f64()
                        ),
                        Err(e) => {
                            failures += 1;
                            println!("FAIL  {}/{}: {e}", model.id, file.path);
                        }
                    }
                }
            }
            if failures > 0 {
                bail!("{failures} file(s) failed verification");
            }
            Ok(())
        }
    }
}
