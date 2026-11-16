//! `glance serve <model>`: verify a pinned vision model, then replace this
//! process with `llama-server` bound to 127.0.0.1.
//!
//! This is the M0 stand-in for the managed supervisor that arrives in M4
//! (`docs/DESIGN.md` §7.1). It already follows the rules that matter: files
//! are hashed before use, the server binds to loopback only on a random port,
//! and it is told to stay offline.

use std::net::TcpListener;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use glance_models::{Kind, Role};

use super::{find_model, manifest, store};

#[derive(clap::Args)]
pub struct Args {
    /// Model id from models.toml.
    id: String,
    /// Port on 127.0.0.1. Default: a random free port.
    #[arg(long)]
    port: Option<u16>,
    /// Context size in tokens.
    #[arg(long, default_value_t = 8192)]
    ctx: u32,
    /// Extra arguments passed to llama-server unchanged.
    #[arg(last = true)]
    extra: Vec<String>,
}

pub fn run(args: Args) -> Result<()> {
    let manifest = manifest()?;
    let model = find_model(&manifest, &args.id)?;
    if model.role != Role::Vlm {
        bail!("`{}` is not a vision-language model", model.id);
    }

    let store = store()?;
    let started = Instant::now();
    store
        .verified_dir(model)
        .with_context(|| format!("run `glance models pull {}` first", model.id))?;
    eprintln!(
        "verified {} file(s) against models.toml in {:.1}s",
        model.files.len(),
        started.elapsed().as_secs_f64()
    );

    let path_of = |kind: Kind| {
        model
            .files
            .iter()
            .find(|f| f.kind == kind)
            .map(|f| store.file_path(model, f))
    };
    let weights = path_of(Kind::Weights).context("model has no weights file")?;
    let mmproj = path_of(Kind::Mmproj).context("model has no mmproj (vision) file")?;

    let port = match args.port {
        Some(port) => port,
        // Ask the OS for a free port, then hand it to llama-server.
        None => TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port(),
    };
    eprintln!(
        "endpoint: http://127.0.0.1:{port}/v1  (model name: {})",
        model.id
    );

    let error = Command::new("llama-server")
        .args(["--host", "127.0.0.1"])
        .args(["--port", &port.to_string()])
        .arg("-m")
        .arg(&weights)
        .arg("--mmproj")
        .arg(&mmproj)
        .args(["--alias", &model.id])
        .args(["--ctx-size", &args.ctx.to_string()])
        .args(["--parallel", "1"])
        .args(["--offline", "--no-webui"])
        .args(&args.extra)
        .exec();
    Err(error).context("could not start llama-server (brew install llama.cpp)")
}
