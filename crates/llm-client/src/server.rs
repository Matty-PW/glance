//! A `llama-server` that glance starts and owns (`docs/DESIGN.md` §7.1).
//!
//! - Binds to 127.0.0.1 on a random free port (CLAUDE.md).
//! - `--offline --no-webui --parallel 1`, one process per tier.
//! - A random per-launch API key, passed through the child's environment
//!   (`LLAMA_API_KEY`, so it never shows in `ps`) and held in memory only.
//! - Start-up waits for `/health`, then refuses a model whose `/props`
//!   does not report vision.
//! - The child's output is discarded unless asked for, so nothing it might
//!   log about a request ends up anywhere.
//! - Dropping [`ManagedServer`] kills the process.

use std::io::Read;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use reqwest::redirect::Policy;

use crate::endpoint::{ApiKey, Endpoint};

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("could not start llama-server (is it installed? `brew install llama.cpp`): {0}")]
    Spawn(std::io::Error),
    #[error("no free port on 127.0.0.1: {0}")]
    Port(std::io::Error),
    #[error("could not create an API key: {0}")]
    Key(std::io::Error),
    #[error("llama-server exited during start-up ({0})")]
    Exited(String),
    #[error("llama-server was not ready after {0} s")]
    Timeout(u64),
    #[error("`{0}` does not accept images (its /props reports no vision); it cannot be a tier")]
    NoVision(String),
    #[error("llama-server: {0}")]
    Http(String),
}

/// What to run.
#[derive(Debug, Clone)]
pub struct ServerSpec {
    /// Verified against `models.toml` by the caller.
    pub weights: PathBuf,
    pub mmproj: PathBuf,
    /// The model name requests use (`--alias`).
    pub alias: String,
    pub ctx: u32,
    /// Show llama-server's own output on stderr (for debugging only).
    pub show_output: bool,
    /// Longest wait for the model to load.
    pub start_timeout: Duration,
}

pub struct ManagedServer {
    child: Child,
    endpoint: Endpoint,
    port: u16,
}

impl std::fmt::Debug for ManagedServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ManagedServer(127.0.0.1:{}, pid {})",
            self.port,
            self.child.id()
        )
    }
}

impl ManagedServer {
    /// Spawn llama-server and wait until it serves a vision model.
    pub async fn start(spec: &ServerSpec) -> Result<Self, ServerError> {
        let key = random_key().map_err(ServerError::Key)?;
        let port = free_port().map_err(ServerError::Port)?;
        let child = Command::new("llama-server")
            .args(["--host", "127.0.0.1"])
            .args(["--port", &port.to_string()])
            .arg("-m")
            .arg(&spec.weights)
            .arg("--mmproj")
            .arg(&spec.mmproj)
            .args(["--alias", &spec.alias])
            .args(["--ctx-size", &spec.ctx.to_string()])
            .args(["--parallel", "1", "--offline", "--no-webui"])
            .env("LLAMA_API_KEY", &key)
            .stdin(Stdio::null())
            .stdout(if spec.show_output {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .stderr(if spec.show_output {
                Stdio::inherit()
            } else {
                Stdio::piped()
            })
            .spawn()
            .map_err(ServerError::Spawn)?;
        let base = format!("http://127.0.0.1:{port}");
        let endpoint = Endpoint::loopback(&format!("{base}/v1"), spec.alias.clone())
            .expect("127.0.0.1 is loopback")
            .with_api_key(ApiKey::new(key.clone()));
        let mut server = Self {
            child,
            endpoint,
            port,
        };
        server.wait_ready(&base, &key, spec).await?;
        // From here on its stderr is not read; stop it filling a pipe.
        drop(server.child.stderr.take());
        Ok(server)
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Whether the process is still running.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    async fn wait_ready(
        &mut self,
        base: &str,
        key: &str,
        spec: &ServerSpec,
    ) -> Result<(), ServerError> {
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| ServerError::Http(e.to_string()))?;
        let started = Instant::now();
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Err(ServerError::Exited(self.exit_detail(status)));
            }
            let health = http
                .get(format!("{base}/health"))
                .bearer_auth(key)
                .send()
                .await;
            if health.is_ok_and(|r| r.status().is_success()) {
                break;
            }
            if started.elapsed() > spec.start_timeout {
                return Err(ServerError::Timeout(spec.start_timeout.as_secs()));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let props: serde_json::Value = http
            .get(format!("{base}/props"))
            .bearer_auth(key)
            .send()
            .await
            .map_err(|e| ServerError::Http(e.to_string()))?
            .json()
            .await
            .map_err(|e| ServerError::Http(e.to_string()))?;
        if props["modalities"]["vision"].as_bool() != Some(true) {
            return Err(ServerError::NoVision(spec.alias.clone()));
        }
        Ok(())
    }

    /// The exit status plus the last lines llama-server wrote (start-up
    /// errors only: nothing has been sent to it yet).
    fn exit_detail(&mut self, status: std::process::ExitStatus) -> String {
        let mut text = String::new();
        if let Some(stderr) = self.child.stderr.as_mut() {
            let _ = stderr.read_to_string(&mut text);
        }
        let tail: Vec<&str> = text.lines().rev().take(3).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        format!("{status}; {}", tail.join(" / "))
    }
}

impl Drop for ManagedServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Ask the OS for a free port. There is a short window before llama-server
/// binds it; on a clash llama-server exits and start-up reports it.
fn free_port() -> std::io::Result<u16> {
    Ok(TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
}

/// 128 random bits from the OS, as hex.
fn random_key() -> std::io::Result<String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_random_hex() {
        let (a, b) = (random_key().unwrap(), random_key().unwrap());
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn free_ports_are_on_loopback_and_nonzero() {
        assert_ne!(free_port().unwrap(), 0);
    }
}
