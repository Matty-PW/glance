//! The `glance` binary. Until the hotkey app exists (M2) this is a set of CLI
//! spikes; see `docs/DESIGN.md` §14.

mod cmd;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "glance",
    version,
    about = "Talk to a local vision model about your screen"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List, download and verify models pinned in models.toml.
    Models {
        #[command(subcommand)]
        action: cmd::models::Action,
    },
    /// Verify a vision model, then run llama-server for it on 127.0.0.1.
    Serve(cmd::serve::Args),
    /// Check that the server reuses a cached image prefix between requests.
    CacheCheck(cmd::cache_check::Args),
    /// M0 end-to-end run: record, transcribe, ask about a PNG, print timings.
    Spike(cmd::spike::Args),
    /// Time a speech model on WAV clips and score word errors.
    SttBench(cmd::stt_bench::Args),
    /// Run the synthetic evaluation set against one model endpoint.
    Bench(cmd::bench::Args),
    /// Record briefly and print microphone level and timing (no transcription).
    MicCheck(cmd::mic_check::Args),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Serve(args) => cmd::serve::run(args),
        Command::SttBench(args) => cmd::stt_bench::run(args),
        Command::MicCheck(args) => cmd::mic_check::run(args),
        other => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(async move {
                match other {
                    Command::Models { action } => cmd::models::run(action).await,
                    Command::CacheCheck(args) => cmd::cache_check::run(args).await,
                    Command::Spike(args) => cmd::spike::run(args).await,
                    Command::Bench(args) => cmd::bench::run(args).await,
                    Command::Serve(_) | Command::SttBench(_) | Command::MicCheck(_) => {
                        unreachable!("handled above")
                    }
                }
            }),
    }
}
