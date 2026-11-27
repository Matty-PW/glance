//! What the settings window shows and changes, as plain data, and the
//! [`SettingsBackend`] the app implements. The window itself (native, in the
//! platform crate) sees only these types; the config file and the running
//! pipeline stay on the app's side.
//!
//! Scope (maintainer, 2026-11-18): voice, answer length, personality,
//! models, think-harder options and hotkeys. Everything else is edited in
//! `config.toml`, which stays the source of truth.

use crate::ResponseLength;

/// The current values.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsValues {
    /// "kokoro" or "system".
    pub engine: String,
    /// Kokoro voice id (or blend) and the system voice (identifier or
    /// language; `None` for the system default).
    pub kokoro_voice: String,
    pub system_voice: Option<String>,
    /// Kokoro pace, 0.5 to 2.0.
    pub speed: f32,
    /// Say maths symbols as words.
    pub math_as_words: bool,
    pub length: ResponseLength,
    /// Preset name (see [`crate::personality::PRESETS`]) and the custom
    /// text, kept even while another preset is chosen.
    pub personality: String,
    pub personality_custom: String,
    pub fast_model: String,
    pub deep_model: Option<String>,
    pub auto_escalate: bool,
    /// "off", "low", "medium" or "xhigh".
    pub deep_effort: String,
    pub deep_thinking_budget: Option<u32>,
    pub deep_idle_mins: u64,
    pub hotkeys: [String; 3],
    /// "glance" (glance's accent colour) or "system" (the macOS accent).
    pub accent: String,
    /// What marks a new screenshot: "glow", "subtle" or "off".
    pub screenshot_indicator: String,
}

/// What can be picked.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SettingsChoices {
    pub kokoro_voices: Vec<String>,
    /// (identifier, label) of the English system voices.
    pub system_voices: Vec<(String, String)>,
    /// Vision models from models.toml that are downloaded.
    pub models: Vec<String>,
}

/// One change from the window.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingsChange {
    Voice {
        engine: String,
        kokoro_voice: String,
        system_voice: Option<String>,
        speed: f32,
    },
    Length(ResponseLength),
    MathAsWords(bool),
    /// Preset name and custom text (used only by "custom").
    Personality {
        preset: String,
        custom: String,
    },
    Think {
        effort: String,
        idle_mins: u64,
    },
    /// Already registered by the platform crate when this arrives.
    Hotkeys([String; 3]),
    /// "glance" or "system"; the platform crate repaints.
    Accent(String),
    /// "glow", "subtle" or "off"; the platform crate applies it.
    ScreenshotIndicator(String),
    /// Applied by restarting glance.
    Models {
        fast: String,
        deep: Option<String>,
        auto_escalate: bool,
        thinking_budget: Option<u32>,
    },
}

/// How a change took effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    Live,
    /// Saved; takes effect when glance restarts.
    NeedsRestart,
}

/// Implemented by the app: read the settings, save and apply changes.
pub trait SettingsBackend: Send + Sync {
    fn current(&self) -> (SettingsValues, SettingsChoices);
    /// What a fresh config.toml would hold ("Restore defaults").
    fn defaults(&self) -> SettingsValues;
    /// Save the change to config.toml and apply what can be applied live.
    /// The error text is shown in the window.
    fn apply(&self, change: SettingsChange) -> Result<Applied, String>;
}

/// One model in `models.toml`, as the models panel shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelRow {
    pub id: String,
    /// "vision", "speech-to-text" or "voice".
    pub role: String,
    pub license: String,
    /// Hugging Face repo and the pinned commit it downloads from.
    pub repo: String,
    pub revision: String,
    /// All files, and what is still missing on disk.
    pub bytes: u64,
    pub bytes_missing: u64,
    pub state: ModelState,
    /// What uses it ("FAST model", "speech recognition", …); such a model
    /// cannot be deleted.
    pub in_use: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ModelState {
    NotDownloaded,
    /// Some files are there (an earlier download stopped).
    Partial {
        present: usize,
        total: usize,
    },
    /// All files are there with their pinned sizes; not hashed this session
    /// (every load hashes them anyway).
    Downloaded,
    /// Hashed this session: every file matches its pinned SHA-256.
    Verified,
    /// The last download or check failed.
    Failed(String),
    /// Checking the files already there before downloading the rest.
    Preparing,
    /// Waiting in the download queue behind another model.
    Queued,
    /// An unfinished download is kept on disk (paused, failed, or glance
    /// quit): `done` of `total` bytes, resumable.
    Paused {
        done: u64,
        total: u64,
    },
    Downloading {
        done: u64,
        total: u64,
    },
    Verifying {
        file: usize,
        files: usize,
    },
}

impl ModelState {
    pub fn is_busy(&self) -> bool {
        matches!(
            self,
            ModelState::Preparing
                | ModelState::Queued
                | ModelState::Downloading { .. }
                | ModelState::Verifying { .. }
        )
    }
}

/// Why a download stopped, in a form that can be put into plain words. It
/// holds no URL, path or server text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadIssue {
    Offline,
    /// A redirect pointed to a host glance does not trust.
    HostRefused(String),
    /// The server answered with this HTTP status.
    Status(u16),
    BadReply,
    /// The finished file did not match its pinned checksum (it was deleted).
    Checksum,
    DiskFull,
    Disk,
    NotEnoughSpace {
        needed: u64,
        free: u64,
    },
    /// Another job (a download or a check of this model) is running.
    Busy(String),
    /// Not a model in models.toml.
    NotPinned(String),
}

impl DownloadIssue {
    /// One or two plain sentences for a window.
    pub fn plain(&self) -> String {
        match self {
            DownloadIssue::Offline => {
                "Couldn't reach huggingface.co. Check your internet connection, then try again."
                    .into()
            }
            DownloadIssue::HostRefused(host) => format!(
                "The download was sent to {host}, which is not one of the Hugging Face servers \
                 glance trusts, so glance stopped it."
            ),
            DownloadIssue::Status(404) => {
                "Hugging Face no longer has this file at the pinned version (HTTP 404).".into()
            }
            DownloadIssue::Status(code @ 500..) => {
                format!("Hugging Face had a problem (HTTP {code}). Try again in a few minutes.")
            }
            DownloadIssue::Status(code) => {
                format!("Hugging Face refused the download (HTTP {code}).")
            }
            DownloadIssue::BadReply => {
                "The server's reply was not what glance expected, so the download stopped. \
                 Try again."
                    .into()
            }
            DownloadIssue::Checksum => {
                "A downloaded file didn't match its pinned checksum, so glance deleted it. \
                 Try again."
                    .into()
            }
            DownloadIssue::DiskFull => "The disk is full. Free some space, then try again.".into(),
            DownloadIssue::Disk => "glance couldn't write the file to disk.".into(),
            DownloadIssue::NotEnoughSpace { needed, free } => format!(
                "Not enough disk space: this needs {:.1} GB (including 2 GB kept free for \
                 macOS) and {:.1} GB is free.",
                *needed as f64 / 1e9,
                *free as f64 / 1e9
            ),
            DownloadIssue::Busy(id) => {
                format!("glance is still working on {id}. Wait for it, or cancel it.")
            }
            DownloadIssue::NotPinned(id) => format!("{id} is not a model in models.toml."),
        }
    }

    /// Stable code for log lines.
    pub fn code(&self) -> &'static str {
        match self {
            DownloadIssue::Offline => "offline",
            DownloadIssue::HostRefused(_) => "host_refused",
            DownloadIssue::Status(_) => "http_status",
            DownloadIssue::BadReply => "bad_reply",
            DownloadIssue::Checksum => "checksum",
            DownloadIssue::DiskFull => "disk_full",
            DownloadIssue::Disk => "disk",
            DownloadIssue::NotEnoughSpace { .. } => "not_enough_space",
            DownloadIssue::Busy(_) => "busy",
            DownloadIssue::NotPinned(_) => "not_pinned",
        }
    }
}

/// Where a queued download is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadPhase {
    Running,
    Paused,
    Failed(DownloadIssue),
    Cancelled,
    Finished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    Waiting,
    Downloading,
    /// All bytes are in; checking size, SHA-256 and format.
    Checking,
    Done,
}

/// One file of a queued download.
#[derive(Debug, Clone, PartialEq)]
pub struct FileProgress {
    pub model: String,
    /// The file's path in models.toml.
    pub path: String,
    pub done: u64,
    pub total: u64,
    pub state: FileState,
}

/// The current (or last) download queue, file by file.
#[derive(Debug, Clone, PartialEq)]
pub struct DownloadProgress {
    pub phase: DownloadPhase,
    pub files: Vec<FileProgress>,
    pub done: u64,
    pub total: u64,
    /// Over the last few seconds; `None` until there is enough to say.
    pub bytes_per_sec: Option<f64>,
    pub remaining: Option<std::time::Duration>,
}

/// An unfinished download kept on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct PartialRow {
    pub name: String,
    /// The pinned model and file it belongs to; `None` for an orphan (no
    /// pinned file has its hash any more).
    pub belongs_to: Option<(String, String)>,
    pub bytes: u64,
}

/// Implemented by the app: list, download, verify and delete models, only
/// ever those pinned in `models.toml`. One job runs at a time.
pub trait ModelsBackend: Send + Sync {
    fn rows(&self) -> Vec<ModelRow>;
    /// Free space where models are stored.
    fn free_bytes(&self) -> Option<u64>;
    /// Start downloading the missing files (the pinned downloader verifies
    /// each one). Returns at once; progress shows in [`rows`](Self::rows).
    fn download(&self, id: &str) -> Result<(), String> {
        self.download_many(&[id.to_owned()]).map_err(|e| e.plain())
    }
    /// Download several models one after another, as one job (setup).
    /// Fails at once if the disk is too small for all of them.
    fn download_many(&self, ids: &[String]) -> Result<(), DownloadIssue>;
    /// The current or last download queue, file by file.
    fn progress(&self) -> Option<DownloadProgress>;
    /// Start re-hashing the files against their pins.
    fn verify(&self, id: &str) -> Result<(), String>;
    /// Stop the running download and keep what has arrived; [`resume`]
    /// carries on from there (also after a restart).
    ///
    /// [`resume`]: Self::resume
    fn pause(&self);
    /// Continue the paused queue, or the unfinished download of `id`.
    fn resume(&self, id: Option<&str>) -> Result<(), DownloadIssue>;
    /// Stop the running job; an unfinished download's partial file is removed.
    fn cancel(&self);
    /// Delete the files. Refused while the model is in use or busy.
    fn delete(&self, id: &str) -> Result<u64, String>;
    /// Unfinished downloads on disk, orphans included.
    fn partials(&self) -> Vec<PartialRow>;
    /// Delete every unfinished download except the running one's. Returns
    /// the bytes freed.
    fn clear_partials(&self) -> u64;
}

#[cfg(test)]
mod tests {
    use super::DownloadIssue;

    #[test]
    fn issues_read_as_plain_words_without_urls() {
        for issue in [
            DownloadIssue::Offline,
            DownloadIssue::HostRefused("evil.example".into()),
            DownloadIssue::Status(404),
            DownloadIssue::Status(503),
            DownloadIssue::Status(403),
            DownloadIssue::BadReply,
            DownloadIssue::Checksum,
            DownloadIssue::DiskFull,
            DownloadIssue::Disk,
            DownloadIssue::NotEnoughSpace {
                needed: 6_400_000_000,
                free: 3_000_000_000,
            },
        ] {
            let text = issue.plain();
            assert!(!text.contains("://"), "{text}");
            assert!(text.ends_with('.'), "{text}");
        }
        assert_eq!(
            DownloadIssue::NotEnoughSpace {
                needed: 6_400_000_000,
                free: 3_000_000_000
            }
            .plain(),
            "Not enough disk space: this needs 6.4 GB (including 2 GB kept free for macOS) and \
             3.0 GB is free."
        );
    }
}
