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
//! - The program is found only by absolute path: an explicit one from the
//!   config, or the first of [`LLAMA_SERVER_DIRS`]. `PATH` is never read,
//!   so what runs does not depend on how glance was launched.
//! - Dropping [`ManagedServer`] kills the process. With a `pid_dir`, a PID
//!   file lets a later glance stop a server left behind by a crash
//!   ([`crate::stale`]).

use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use reqwest::redirect::Policy;

use crate::endpoint::{ApiKey, Endpoint};

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error(
        "llama-server not found in {}; install it with `brew install llama.cpp`, or set \
         `llama_server` in [models] of config.toml to its absolute path",
        LLAMA_SERVER_DIRS.join(" or ")
    )]
    NotFound,
    #[error("the configured llama_server `{0}` is not an executable file")]
    NotExecutable(PathBuf),
    #[error("the configured llama_server `{0}` must be an absolute path")]
    NotAbsolute(PathBuf),
    #[error("could not start llama-server: {0}")]
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

/// Where llama-server is looked for, in order (Homebrew on Apple Silicon,
/// then on Intel or by hand). Never `PATH`.
pub const LLAMA_SERVER_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

/// The llama-server to run: `explicit` (absolute, executable) if given, and
/// then nothing else; otherwise the first executable `llama-server` in
/// [`LLAMA_SERVER_DIRS`].
pub fn find_llama_server(explicit: Option<&Path>) -> Result<PathBuf, ServerError> {
    find_in(explicit, LLAMA_SERVER_DIRS, is_executable_file)
}

fn find_in(
    explicit: Option<&Path>,
    dirs: &[&str],
    executable: impl Fn(&Path) -> bool,
) -> Result<PathBuf, ServerError> {
    if let Some(path) = explicit {
        if !path.is_absolute() {
            return Err(ServerError::NotAbsolute(path.to_owned()));
        }
        return if executable(path) {
            Ok(path.to_owned())
        } else {
            Err(ServerError::NotExecutable(path.to_owned()))
        };
    }
    dirs.iter()
        .map(|dir| Path::new(dir).join("llama-server"))
        .find(|path| executable(path))
        .ok_or(ServerError::NotFound)
}

/// A regular file (after following links, as Homebrew's are) with an
/// execute bit.
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// What to run.
#[derive(Debug, Clone)]
pub struct ServerSpec {
    /// The llama-server binary, from [`find_llama_server`].
    pub program: PathBuf,
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
    /// More llama-server flags, e.g. `--spec-type draft-mtp`. Fixed text
    /// from code or config, never from the screen or a model.
    pub extra_args: Vec<String>,
    /// Where to write this server's PID file (see [`crate::stale`]).
    pub pid_dir: Option<PathBuf>,
}

pub struct ManagedServer {
    child: Child,
    endpoint: Endpoint,
    port: u16,
    pid_file: Option<PathBuf>,
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
        let child = Command::new(&spec.program)
            .args(["--host", "127.0.0.1"])
            .args(["--port", &port.to_string()])
            .arg("-m")
            .arg(&spec.weights)
            .arg("--mmproj")
            .arg(&spec.mmproj)
            .args(["--alias", &spec.alias])
            .args(["--ctx-size", &spec.ctx.to_string()])
            .args(["--parallel", "1", "--offline", "--no-webui"])
            .args(&spec.extra_args)
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
        let pid_file = spec.pid_dir.as_ref().and_then(|dir| {
            let file = crate::stale::PidFile {
                llama_pid: child.id(),
                owner_pid: std::process::id(),
                port,
                alias: spec.alias.clone(),
            };
            file.write(dir)
                .map_err(|e| {
                    eprintln!(
                        "glance: could not write a PID file in {}: {e}",
                        dir.display()
                    )
                })
                .ok()
        });
        let mut server = Self {
            child,
            endpoint,
            port,
            pid_file,
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

    pub fn pid(&self) -> u32 {
        self.child.id()
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
        if let Some(path) = &self.pid_file {
            let _ = std::fs::remove_file(path);
        }
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
    use std::path::Path;

    #[test]
    fn llama_server_is_found_only_by_absolute_path() {
        let dirs = ["/opt/homebrew/bin", "/usr/local/bin"];
        let only = |wanted: &'static str| move |p: &Path| p == Path::new(wanted);
        assert_eq!(
            super::find_in(None, &dirs, only("/usr/local/bin/llama-server")).unwrap(),
            Path::new("/usr/local/bin/llama-server")
        );
        // Homebrew's comes first when both exist.
        assert_eq!(
            super::find_in(None, &dirs, |_| true).unwrap(),
            Path::new("/opt/homebrew/bin/llama-server")
        );
        assert!(matches!(
            super::find_in(None, &dirs, |_| false),
            Err(super::ServerError::NotFound)
        ));
        // An explicit path is the only one tried, and must be absolute.
        let mine = Path::new("/opt/llama.cpp/build/bin/llama-server");
        assert_eq!(
            super::find_in(
                Some(mine),
                &dirs,
                only("/opt/llama.cpp/build/bin/llama-server")
            )
            .unwrap(),
            mine
        );
        assert!(matches!(
            super::find_in(Some(mine), &dirs, only("/opt/homebrew/bin/llama-server")),
            Err(super::ServerError::NotExecutable(_))
        ));
        assert!(matches!(
            super::find_in(Some(Path::new("bin/llama-server")), &dirs, |_| true),
            Err(super::ServerError::NotAbsolute(_))
        ));
    }

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
