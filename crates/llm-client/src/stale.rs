//! Cleaning up `llama-server` processes left behind by a glance that did not
//! exit cleanly (a crash, a force quit, `kill -9`). Quit and Ctrl-C already
//! stop them; this covers the rest.
//!
//! Every managed server gets a PID file in the run directory
//! (`llama-server-<port>.pid`: its PID, its port, the owning glance PID). A
//! clean shutdown removes it. At start-up glance reads every file there, and
//! when the owner is gone it stops the server, but only after checking that
//! the PID still belongs to a `llama-server` on 127.0.0.1 with that exact
//! port, so a PID the system has since reused for something else is never
//! touched.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// What one PID file held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PidFile {
    pub llama_pid: u32,
    pub owner_pid: u32,
    pub port: u16,
    pub alias: String,
}

impl PidFile {
    pub fn path(dir: &Path, port: u16) -> PathBuf {
        dir.join(format!("llama-server-{port}.pid"))
    }

    fn render(&self) -> String {
        format!(
            "llama_pid={}\nowner_pid={}\nport={}\nalias={}\n",
            self.llama_pid, self.owner_pid, self.port, self.alias
        )
    }

    fn parse(text: &str) -> Option<Self> {
        let field = |key: &str| {
            text.lines()
                .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
                .map(str::trim)
        };
        Some(Self {
            llama_pid: field("llama_pid")?.parse().ok()?,
            owner_pid: field("owner_pid")?.parse().ok()?,
            port: field("port")?.parse().ok()?,
            alias: field("alias").unwrap_or_default().to_owned(),
        })
    }

    pub fn write(&self, dir: &Path) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(dir)?;
        let path = Self::path(dir, self.port);
        std::fs::write(&path, self.render())?;
        Ok(path)
    }
}

/// What was done with one PID file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cleanup {
    /// Its glance is still running (another instance): left alone.
    OwnerRunning { port: u16 },
    /// The orphaned server was stopped.
    Stopped { port: u16, pid: u32, alias: String },
    /// The server had already exited.
    AlreadyGone { port: u16 },
    /// The PID now belongs to some other process: not touched.
    NotOurs { port: u16, pid: u32 },
    /// Unreadable file: removed.
    Malformed { path: PathBuf },
}

/// The process table, behind a trait so the decisions can be tested.
pub trait Processes {
    /// The full command line of a running process, or `None`.
    fn command_line(&self, pid: u32) -> Option<String>;
    /// Stop a process: SIGTERM, then SIGKILL if it lingers.
    fn terminate(&self, pid: u32);
}

/// The real process table (`ps` and `kill`).
pub struct System;

impl Processes for System {
    fn command_line(&self, pid: u32) -> Option<String> {
        let out = std::process::Command::new("/bin/ps")
            .args(["-p", &pid.to_string(), "-o", "args="])
            .output()
            .ok()?;
        let line = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        (out.status.success() && !line.is_empty()).then_some(line)
    }

    fn terminate(&self, pid: u32) {
        let signal = |sig: &str| {
            let _ = std::process::Command::new("/bin/kill")
                .args([sig, &pid.to_string()])
                .status();
        };
        signal("-TERM");
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if self.command_line(pid).is_none() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        signal("-KILL");
    }
}

/// Whether `command_line` is a llama-server glance started on `port`.
fn is_our_server(command_line: &str, port: u16) -> bool {
    let args: Vec<&str> = command_line.split_whitespace().collect();
    let has_pair = |flag: &str, value: &str| args.windows(2).any(|w| w[0] == flag && w[1] == value);
    args.first()
        .is_some_and(|exe| exe.rsplit('/').next() == Some("llama-server"))
        && has_pair("--host", "127.0.0.1")
        && has_pair("--port", &port.to_string())
}

/// Stop servers whose glance is gone, and remove their PID files. Files of
/// a running glance (this process excluded) are left alone.
pub fn clean_stale(dir: &Path) -> Vec<Cleanup> {
    clean_stale_with(dir, &System, std::process::id())
}

pub fn clean_stale_with(dir: &Path, processes: &dyn Processes, me: u32) -> Vec<Cleanup> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut done = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !(name.starts_with("llama-server-") && name.ends_with(".pid")) {
            continue;
        }
        let Some(file) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| PidFile::parse(&t))
        else {
            let _ = std::fs::remove_file(&path);
            done.push(Cleanup::Malformed { path });
            continue;
        };
        let owner_running = file.owner_pid != me
            && processes
                .command_line(file.owner_pid)
                .is_some_and(|c| c.contains("glance"));
        if owner_running {
            done.push(Cleanup::OwnerRunning { port: file.port });
            continue;
        }
        let outcome = match processes.command_line(file.llama_pid) {
            None => Cleanup::AlreadyGone { port: file.port },
            Some(line) if is_our_server(&line, file.port) => {
                processes.terminate(file.llama_pid);
                Cleanup::Stopped {
                    port: file.port,
                    pid: file.llama_pid,
                    alias: file.alias.clone(),
                }
            }
            Some(_) => Cleanup::NotOurs {
                port: file.port,
                pid: file.llama_pid,
            },
        };
        let _ = std::fs::remove_file(&path);
        done.push(outcome);
    }
    done
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use super::*;

    struct Fake {
        running: HashMap<u32, String>,
        terminated: RefCell<Vec<u32>>,
    }

    impl Processes for Fake {
        fn command_line(&self, pid: u32) -> Option<String> {
            self.running.get(&pid).cloned()
        }
        fn terminate(&self, pid: u32) {
            self.terminated.borrow_mut().push(pid);
        }
    }

    fn server_line(port: u16) -> String {
        format!(
            "/opt/homebrew/bin/llama-server --host 127.0.0.1 --port {port} -m /x.gguf --offline"
        )
    }

    fn dir() -> PathBuf {
        // A counter, not the clock: tests run in parallel and the macOS
        // clock only has microsecond resolution, so two could share a dir.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!(
            "glance-stale-test-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn file(llama: u32, owner: u32, port: u16) -> PidFile {
        PidFile {
            llama_pid: llama,
            owner_pid: owner,
            port,
            alias: "m".into(),
        }
    }

    #[test]
    fn an_orphan_is_stopped_and_its_file_removed() {
        let d = dir();
        file(100, 50, 9001).write(&d).unwrap();
        let fake = Fake {
            running: HashMap::from([(100, server_line(9001))]),
            terminated: RefCell::new(Vec::new()),
        };
        let done = clean_stale_with(&d, &fake, 1);
        assert_eq!(
            done,
            [Cleanup::Stopped {
                port: 9001,
                pid: 100,
                alias: "m".into()
            }]
        );
        assert_eq!(*fake.terminated.borrow(), [100]);
        assert!(!PidFile::path(&d, 9001).exists());
    }

    #[test]
    fn a_running_owner_keeps_its_server() {
        let d = dir();
        file(100, 50, 9002).write(&d).unwrap();
        let fake = Fake {
            running: HashMap::from([
                (100, server_line(9002)),
                (
                    50,
                    "/Applications/Glance.app/Contents/MacOS/glance app".into(),
                ),
            ]),
            terminated: RefCell::new(Vec::new()),
        };
        assert_eq!(
            clean_stale_with(&d, &fake, 1),
            [Cleanup::OwnerRunning { port: 9002 }]
        );
        assert!(fake.terminated.borrow().is_empty());
        assert!(PidFile::path(&d, 9002).exists());
    }

    #[test]
    fn a_reused_pid_is_never_touched() {
        let d = dir();
        file(100, 50, 9003).write(&d).unwrap();
        for other in [
            "/usr/bin/vim notes.txt".to_owned(),
            // Another llama-server, but not the one we started (other port).
            server_line(9999),
            // Same port, but not on loopback.
            "/opt/homebrew/bin/llama-server --host 0.0.0.0 --port 9003".to_owned(),
        ] {
            file(100, 50, 9003).write(&d).unwrap();
            let fake = Fake {
                running: HashMap::from([(100, other)]),
                terminated: RefCell::new(Vec::new()),
            };
            assert_eq!(
                clean_stale_with(&d, &fake, 1),
                [Cleanup::NotOurs {
                    port: 9003,
                    pid: 100
                }]
            );
            assert!(fake.terminated.borrow().is_empty());
        }
    }

    #[test]
    fn gone_servers_and_bad_files_are_just_removed() {
        let d = dir();
        file(100, 50, 9004).write(&d).unwrap();
        std::fs::write(d.join("llama-server-1.pid"), "garbage").unwrap();
        std::fs::write(d.join("unrelated.txt"), "keep me").unwrap();
        let fake = Fake {
            running: HashMap::new(),
            terminated: RefCell::new(Vec::new()),
        };
        let mut done = clean_stale_with(&d, &fake, 1);
        done.sort_by_key(|c| format!("{c:?}"));
        assert_eq!(done.len(), 2);
        assert!(done.contains(&Cleanup::AlreadyGone { port: 9004 }));
        assert!(d.join("unrelated.txt").exists());
        assert!(std::fs::read_dir(&d).unwrap().count() == 1);
    }

    #[test]
    fn pid_files_round_trip() {
        let f = file(7, 8, 9);
        assert_eq!(PidFile::parse(&f.render()), Some(f));
        assert_eq!(PidFile::parse("llama_pid=x"), None);
    }

    #[test]
    fn the_real_process_table_sees_this_process() {
        let me = System.command_line(std::process::id()).unwrap();
        assert!(!me.is_empty());
        assert_eq!(System.command_line(u32::MAX - 1), None);
    }
}
