//! Starting FAST and DEEP: either a `llama-server` glance runs itself for a
//! model pinned in `models.toml`, or an endpoint the user runs (loopback
//! only, enforced by `Endpoint::loopback`).
//!
//! Managed servers are kept in a process-wide list so Quit can stop them:
//! `NSApplication::terminate` exits without running destructors.

use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use glance_llm_client::server::{ManagedServer, ServerSpec};
use glance_llm_client::{Endpoint, OpenAiClient};
use glance_models::{Kind, Role};

use super::super::{find_model, manifest, store};

static SERVERS: Mutex<Vec<ManagedServer>> = Mutex::new(Vec::new());

/// Stop every managed server (Quit).
pub fn shutdown() {
    let servers = std::mem::take(&mut *SERVERS.lock().unwrap_or_else(|p| p.into_inner()));
    drop(servers); // ManagedServer kills its process on drop
}

/// Where one tier runs.
#[derive(Debug, Clone)]
pub enum Source {
    /// glance runs llama-server for this models.toml id.
    Managed {
        id: String,
        ctx: u32,
        /// Extra llama-server flags (MTP, thinking budget), from config.
        args: Vec<String>,
    },
    /// The user's own server.
    External { endpoint: String, model: String },
}

impl Source {
    pub fn label(&self) -> String {
        match self {
            Source::Managed { id, .. } => format!("{id} (run by glance)"),
            Source::External { endpoint, model } => format!("{model} at {endpoint}"),
        }
    }

    /// Check what can be checked without starting anything.
    pub fn check(&self) -> Result<()> {
        match self {
            Source::Managed { id, .. } => {
                let manifest = manifest()?;
                let model = find_model(&manifest, id)?;
                if model.role != Role::Vlm {
                    bail!("`{id}` is not a vision-language model");
                }
            }
            Source::External { endpoint, model } => {
                Endpoint::loopback(endpoint, model.clone())?;
            }
        }
        Ok(())
    }
}

/// A ready tier.
pub struct Tier {
    pub client: OpenAiClient,
    /// llama-server accepts the escalation grammar; other servers may not.
    pub grammar: bool,
}

pub async fn start(source: Source) -> Result<Tier> {
    match source {
        Source::External { endpoint, model } => Ok(Tier {
            client: OpenAiClient::new(Endpoint::loopback(&endpoint, model)?)?,
            grammar: false,
        }),
        Source::Managed { id, ctx, args } => {
            let spec = tokio::task::spawn_blocking(move || -> Result<ServerSpec> {
                let manifest = manifest()?;
                let model = find_model(&manifest, &id)?;
                let dir = store()?
                    .verified_dir(model)
                    .with_context(|| format!("run `glance models pull {id}` first"))?;
                let file = |kind: Kind| {
                    model
                        .files
                        .iter()
                        .find(|f| f.kind == kind)
                        .map(|f| dir.join(&f.path))
                        .with_context(|| format!("{id} has no {kind:?} file"))
                };
                Ok(ServerSpec {
                    weights: file(Kind::Weights)?,
                    mmproj: file(Kind::Mmproj)?,
                    alias: id.clone(),
                    ctx,
                    show_output: false,
                    // Large DEEP models take a while to load.
                    start_timeout: Duration::from_secs(300),
                    extra_args: args,
                })
            })
            .await??;
            let server = ManagedServer::start(&spec).await?;
            eprintln!(
                "glance: {} serving on 127.0.0.1:{}",
                spec.alias,
                server.port()
            );
            let client = OpenAiClient::new(server.endpoint().clone())?;
            SERVERS
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(server);
            Ok(Tier {
                client,
                grammar: true,
            })
        }
    }
}
