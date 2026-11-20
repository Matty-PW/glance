//! The `glance` binary: the menu-bar app (`glance app`) plus the CLI spikes
//! and checks used to measure each stage; see `docs/DESIGN.md` §14.

mod cmd;
mod config;
mod config_edit;

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
    /// The menu-bar app: hold Ctrl+Opt+G, ask about the screen, hear the answer.
    App(cmd::app::Args),
    /// List, download and verify models pinned in models.toml.
    Models {
        #[command(subcommand)]
        action: cmd::models::Action,
    },
    /// Verify a vision model, then run llama-server for it on 127.0.0.1.
    Serve(cmd::serve::Args),
    /// Start a managed llama-server and check the escalation grammar works.
    ServerCheck(cmd::server_check::Args),
    /// Check that the server reuses a cached image prefix between requests.
    CacheCheck(cmd::cache_check::Args),
    /// M0 end-to-end run: record, transcribe, ask about a PNG, print timings.
    Spike(cmd::spike::Args),
    /// Time a speech model on WAV clips and score word errors.
    SttBench(cmd::stt_bench::Args),
    /// FAST-only comparison: memory, first word on a fresh capture and warm,
    /// answers side by side.
    FastBench(cmd::fast_bench::Args),
    /// DEEP bake-off: FAST's escalation decision, then DEEP's thinking, answer
    /// and first-word times per reasoning effort.
    DeepBench(cmd::deep_bench::Args),
    /// Run the synthetic evaluation set against one model endpoint.
    Bench(cmd::bench::Args),
    /// Capture the active display and print sizes and timings (no image is kept).
    CaptureCheck(cmd::capture_check::Args),
    /// Record briefly and print microphone level and timing (no transcription).
    MicCheck(cmd::mic_check::Args),
    /// Compare Kokoro precisions and the system voice: synthesis speed and
    /// time to first audio.
    TtsBench(cmd::tts_bench::Args),
    /// Speak text sentence by sentence (Kokoro or the system voice), to audition voices.
    Say(cmd::say::Args),
    /// List the Kokoro voices and the English system voices.
    Voices {
        #[command(subcommand)]
        action: Option<cmd::say::VoicesAction>,
    },
    /// Draw glance's own windows (demo text) to PNGs for layout review.
    UiSnapshots(cmd::ui_snapshots::Args),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let runtime = || {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
    };
    match cli.command {
        // Owns the main thread for the app's run loop until Quit.
        Command::App(args) => cmd::app::run(args),
        Command::Serve(args) => cmd::serve::run(args),
        Command::SttBench(args) => cmd::stt_bench::run(args),
        Command::MicCheck(args) => cmd::mic_check::run(args),
        Command::CaptureCheck(args) => cmd::capture_check::run(args),
        Command::Say(args) => cmd::say::run(args),
        Command::Voices { action } => cmd::say::voices(action),
        Command::UiSnapshots(args) => cmd::ui_snapshots::run(args),
        Command::Spike(args) => {
            // The spike speaks, and macOS delivers speech events on the main
            // thread's run loop. So the pipeline runs on the runtime's worker
            // threads while this (main) thread keeps the run loop turning.
            let runtime = runtime()?;
            let task = runtime.spawn(cmd::spike::run(args));
            glance_macos::system_voice::run_main_loop_until(|| task.is_finished());
            runtime.block_on(task)?
        }
        Command::TtsBench(args) => {
            // The system voice reports on the main run loop (see Spike).
            let runtime = runtime()?;
            let task = runtime.spawn(cmd::tts_bench::run(args));
            glance_macos::system_voice::run_main_loop_until(|| task.is_finished());
            runtime.block_on(task)?
        }
        Command::Models { action } => runtime()?.block_on(cmd::models::run(action)),
        Command::ServerCheck(args) => runtime()?.block_on(cmd::server_check::run(args)),
        Command::CacheCheck(args) => runtime()?.block_on(cmd::cache_check::run(args)),
        Command::FastBench(args) => {
            // Speaks (at volume 0) to time the voice; see Spike.
            let runtime = runtime()?;
            let task = runtime.spawn(cmd::fast_bench::run(args));
            glance_macos::system_voice::run_main_loop_until(|| task.is_finished());
            runtime.block_on(task)?
        }
        Command::DeepBench(args) => {
            // Speaks (at volume 0) to time the voice; see Spike.
            let runtime = runtime()?;
            let task = runtime.spawn(cmd::deep_bench::run(args));
            glance_macos::system_voice::run_main_loop_until(|| task.is_finished());
            runtime.block_on(task)?
        }
        Command::Bench(args) => runtime()?.block_on(cmd::bench::run(args)),
    }
}
