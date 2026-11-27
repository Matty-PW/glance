//! The models panel's backend (and setup's downloads): list, download,
//! verify and delete the models pinned in `models.toml`, through the same
//! downloader and store as `glance models` (HTTPS from huggingface.co,
//! allowlisted CDN redirects, pinned size and SHA-256 checked before a file
//! enters the store). Nothing outside the manifest can be downloaded or
//! deleted from here.
//!
//! One job runs at a time: a download queue (one or more models, one after
//! another) or a check. Windows poll [`ModelsBackend::rows`] and
//! [`ModelsBackend::progress`].
//!
//! Unfinished downloads: Pause, a failure and Quit keep the partial file, and
//! the next download of that model resumes it (the downloader re-hashes it
//! first). Cancel deletes it. "Clear" deletes every partial but the running
//! one's.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use glance_models::{Failure, Manifest, Model, ModelStore, Role};
use glance_orchestrator::settings::{
    DownloadIssue, DownloadPhase, DownloadProgress, FileProgress, FileState, ModelRow, ModelState,
    ModelsBackend, PartialRow,
};

/// Kept free beyond a download's size, so a full disk never stops macOS.
pub const DISK_MARGIN: u64 = 2_000_000_000;

/// Speed is measured over this window.
const RATE_WINDOW: Duration = Duration::from_secs(5);

/// What uses which model: `(id, label)` pairs.
pub type InUse = Box<dyn Fn() -> Vec<(String, String)> + Send + Sync>;

pub struct ModelsManager {
    manifest: Manifest,
    store: ModelStore,
    runtime: tokio::runtime::Handle,
    in_use: InUse,
    job: Mutex<Option<Job>>,
    /// The last check or download result per model, this session.
    checked: Mutex<HashMap<String, Result<(), String>>>,
    /// The current or last download queue.
    progress: Arc<Mutex<Option<DownloadProgress>>>,
    /// The ids of the last paused or failed queue, for Resume.
    paused: Mutex<Vec<String>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobKind {
    Download,
    Verify,
}

struct Job {
    kind: JobKind,
    /// The models of the job, in order.
    queue: Vec<String>,
    /// The one being worked on now.
    current: Arc<Mutex<String>>,
    /// Its row state.
    state: Arc<Mutex<ModelState>>,
    cancel: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Download speed over the last few seconds.
#[derive(Default)]
pub(crate) struct Rate {
    samples: VecDeque<(Instant, u64)>,
}

impl Rate {
    /// `fetched`: bytes received this session (it only goes back when a
    /// file starts over, which resets the measurement).
    pub(crate) fn push(&mut self, at: Instant, fetched: u64) {
        if self.samples.back().is_some_and(|&(_, last)| fetched < last) {
            self.samples.clear();
        }
        self.samples.push_back((at, fetched));
        while self.samples.len() > 2
            && self
                .samples
                .front()
                .is_some_and(|&(t, _)| at.duration_since(t) > RATE_WINDOW)
        {
            self.samples.pop_front();
        }
    }

    /// Bytes per second, once at least a second has been measured.
    pub(crate) fn per_sec(&self) -> Option<f64> {
        let (&(t0, b0), &(t1, b1)) = (self.samples.front()?, self.samples.back()?);
        let secs = t1.duration_since(t0).as_secs_f64();
        (secs >= 1.0).then(|| (b1 - b0) as f64 / secs)
    }
}

/// Time left at `rate` bytes per second.
pub(crate) fn remaining(done: u64, total: u64, rate: Option<f64>) -> Option<Duration> {
    let rate = rate.filter(|r| *r > 1.0)?;
    Some(Duration::from_secs_f64(
        total.saturating_sub(done) as f64 / rate,
    ))
}

fn issue_of(failure: Failure) -> DownloadIssue {
    match failure {
        Failure::Offline => DownloadIssue::Offline,
        Failure::HostRefused(host) => DownloadIssue::HostRefused(host),
        Failure::Status(code) => DownloadIssue::Status(code),
        Failure::BadReply => DownloadIssue::BadReply,
        Failure::Checksum => DownloadIssue::Checksum,
        Failure::DiskFull => DownloadIssue::DiskFull,
        Failure::Disk => DownloadIssue::Disk,
    }
}

impl ModelsManager {
    pub fn new(
        manifest: Manifest,
        store: ModelStore,
        runtime: tokio::runtime::Handle,
        in_use: InUse,
    ) -> Arc<Self> {
        Arc::new(Self {
            manifest,
            store,
            runtime,
            in_use,
            job: Mutex::new(None),
            checked: Mutex::new(HashMap::new()),
            progress: Arc::new(Mutex::new(None)),
            paused: Mutex::new(Vec::new()),
        })
    }

    fn model(&self, id: &str) -> Result<&Model, String> {
        self.manifest
            .model(id)
            .ok_or_else(|| format!("{id} is not in models.toml"))
    }

    /// The model the running job is working on, if it has not finished.
    fn busy(&self) -> Option<String> {
        let mut job = lock(&self.job);
        if job.as_ref().is_some_and(|j| j.task.is_finished()) {
            *job = None;
        }
        job.as_ref().map(|j| lock(&j.current).clone())
    }

    /// Bytes of each file already on disk: the whole file when it has its
    /// pinned size, else what an unfinished download kept.
    fn on_disk(&self, model: &Model) -> Vec<(u64, bool)> {
        model
            .files
            .iter()
            .map(|f| {
                let complete = std::fs::metadata(self.store.file_path(model, f))
                    .is_ok_and(|m| m.is_file() && m.len() == f.size);
                if complete {
                    (f.size, true)
                } else {
                    let kept = std::fs::metadata(self.store.partial_path(f))
                        .map_or(0, |m| m.len().min(f.size));
                    (kept, false)
                }
            })
            .collect()
    }

    fn missing_bytes(&self, model: &Model) -> u64 {
        model
            .files
            .iter()
            .filter(|f| {
                std::fs::metadata(self.store.file_path(model, f))
                    .map_or(true, |m| m.len() != f.size)
            })
            .map(|f| f.size)
            .sum()
    }

    /// Bytes still to fetch, counting what partials already hold.
    fn to_fetch(&self, model: &Model) -> u64 {
        let total: u64 = model.files.iter().map(|f| f.size).sum();
        total.saturating_sub(self.on_disk(model).iter().map(|(b, _)| b).sum())
    }

    fn finish(&self, id: &str, result: Result<(), String>) {
        if let Err(e) = &result {
            eprintln!("glance: models: {id}: {e}");
        }
        lock(&self.checked).insert(id.to_owned(), result);
    }

    /// Start the download queue `ids`.
    fn start_download(self: &Arc<Self>, ids: &[String]) -> Result<(), DownloadIssue> {
        if let Some(other) = self.busy() {
            let same = lock(&self.job)
                .as_ref()
                .is_some_and(|j| j.kind == JobKind::Download && j.queue == ids);
            return if same {
                Ok(())
            } else {
                Err(DownloadIssue::Busy(other))
            };
        }
        let models: Vec<Model> = ids
            .iter()
            .map(|id| {
                self.manifest
                    .model(id)
                    .cloned()
                    .ok_or_else(|| DownloadIssue::NotPinned(id.clone()))
            })
            .collect::<Result<_, _>>()?;
        let needed: u64 = models.iter().map(|m| self.to_fetch(m)).sum();
        if needed > 0
            && let Some(free) = free_bytes(self.store.root())
            && free < needed + DISK_MARGIN
        {
            let issue = DownloadIssue::NotEnoughSpace {
                needed: needed + DISK_MARGIN,
                free,
            };
            eprintln!("glance: models: download code={}", issue.code());
            return Err(issue);
        }
        // The starting picture: whole files done, partials counted.
        let mut files = Vec::new();
        for model in &models {
            for (file, (bytes, complete)) in model.files.iter().zip(self.on_disk(model)) {
                files.push(FileProgress {
                    model: model.id.clone(),
                    path: file.path.clone(),
                    done: bytes,
                    total: file.size,
                    state: if complete {
                        FileState::Done
                    } else {
                        FileState::Waiting
                    },
                });
            }
        }
        let start: Vec<u64> = files.iter().map(|f| f.done).collect();
        let total = files.iter().map(|f| f.total).sum();
        *lock(&self.progress) = Some(DownloadProgress {
            phase: DownloadPhase::Running,
            done: files.iter().map(|f| f.done).sum(),
            total,
            files,
            bytes_per_sec: None,
            remaining: None,
        });
        lock(&self.paused).clear();
        for model in &models {
            lock(&self.checked).remove(&model.id);
            eprintln!(
                "glance: models: downloading {} from huggingface.co/{} at {}",
                model.id, model.repo, model.revision
            );
        }
        let first = models[0].id.clone();
        let current = Arc::new(Mutex::new(first));
        let state = Arc::new(Mutex::new(ModelState::Preparing));
        let cancel = Arc::new(AtomicBool::new(false));
        let m = Arc::clone(self);
        let (job_current, job_state) = (Arc::clone(&current), Arc::clone(&state));
        let queue: Vec<String> = ids.to_vec();
        let task = self.runtime.spawn(async move {
            let mut rate = Rate::default();
            let mut offset = 0; // index of this model's first file
            for model in &models {
                *lock(&job_current) = model.id.clone();
                *lock(&job_state) = ModelState::Preparing;
                let model_total: u64 = model.files.iter().map(|f| f.size).sum();
                let result = glance_models::pull(&m.store, model, |file, done, _| {
                    let index = offset
                        + model
                            .files
                            .iter()
                            .position(|f| f.path == file.path)
                            .unwrap_or(0);
                    let mut guard = lock(&m.progress);
                    let Some(p) = guard.as_mut() else { return };
                    // Earlier files of this model have passed their check.
                    for f in &mut p.files[offset..index] {
                        f.done = f.total;
                        f.state = FileState::Done;
                    }
                    let entry = &mut p.files[index];
                    entry.done = done;
                    entry.state = if done == entry.total {
                        FileState::Checking
                    } else {
                        FileState::Downloading
                    };
                    p.done = p.files.iter().map(|f| f.done).sum();
                    let fetched = p
                        .files
                        .iter()
                        .zip(&start)
                        .map(|(f, s)| f.done.saturating_sub(*s))
                        .sum();
                    rate.push(Instant::now(), fetched);
                    p.bytes_per_sec = rate.per_sec();
                    p.remaining = remaining(p.done, p.total, p.bytes_per_sec);
                    let model_done = p.files[offset..offset + model.files.len()]
                        .iter()
                        .map(|f| f.done)
                        .sum();
                    *lock(&job_state) = ModelState::Downloading {
                        done: model_done,
                        total: model_total,
                    };
                })
                .await;
                match result {
                    Ok(_) => {
                        if let Some(p) = lock(&m.progress).as_mut() {
                            for f in &mut p.files[offset..offset + model.files.len()] {
                                f.done = f.total;
                                f.state = FileState::Done;
                            }
                            p.done = p.files.iter().map(|f| f.done).sum();
                        }
                        // pull checks every file against its pin.
                        m.finish(&model.id, Ok(()));
                    }
                    Err(e) => {
                        let issue = issue_of(e.failure());
                        if issue == DownloadIssue::BadReply {
                            // A broken partial reply would only repeat.
                            m.store.remove_partials(model);
                        }
                        eprintln!(
                            "glance: models: {} download code={}",
                            model.id,
                            issue.code()
                        );
                        m.finish(&model.id, Err(issue.plain()));
                        if let Some(p) = lock(&m.progress).as_mut() {
                            p.phase = DownloadPhase::Failed(issue);
                            p.bytes_per_sec = None;
                            p.remaining = None;
                        }
                        *lock(&m.paused) = models.iter().map(|m| m.id.clone()).collect();
                        return;
                    }
                }
                offset += model.files.len();
            }
            if let Some(p) = lock(&m.progress).as_mut() {
                p.phase = DownloadPhase::Finished;
                p.bytes_per_sec = None;
                p.remaining = Some(Duration::ZERO);
            }
        });
        *lock(&self.job) = Some(Job {
            kind: JobKind::Download,
            queue,
            current,
            state,
            cancel,
            task,
        });
        Ok(())
    }

    /// Stop the running job. `keep`: leave its partial files for a resume.
    fn stop(&self, keep: bool) {
        let Some(job) = lock(&self.job).take() else {
            return;
        };
        job.cancel.store(true, Ordering::Relaxed);
        job.task.abort();
        if job.kind != JobKind::Download {
            return;
        }
        if let Some(p) = lock(&self.progress).as_mut()
            && p.phase == DownloadPhase::Running
        {
            p.phase = if keep {
                DownloadPhase::Paused
            } else {
                DownloadPhase::Cancelled
            };
            p.bytes_per_sec = None;
            p.remaining = None;
        }
        if keep {
            *lock(&self.paused) = job.queue.clone();
            eprintln!("glance: models: download paused");
            return;
        }
        let mut freed = 0;
        for id in &job.queue {
            if let Some(model) = self.manifest.model(id) {
                freed += self.store.remove_partials(model);
            }
        }
        eprintln!(
            "glance: models: download cancelled ({:.1} GB of unfinished download removed)",
            freed as f64 / 1e9
        );
    }
}

/// The trait is implemented on the `Arc` because jobs keep a reference.
pub struct Models(pub Arc<ModelsManager>);

impl ModelsBackend for Models {
    fn rows(&self) -> Vec<ModelRow> {
        let m = &self.0;
        let in_use = (m.in_use)();
        let busy = m.busy();
        let job = lock(&m.job).as_ref().map(|j| {
            let current = lock(&j.current).clone();
            let ahead: Vec<String> = j
                .queue
                .iter()
                .skip_while(|id| **id != current)
                .skip(1)
                .cloned()
                .collect();
            (lock(&j.state).clone(), ahead)
        });
        let checked = lock(&m.checked).clone();
        m.manifest
            .models()
            .iter()
            .map(|model| {
                let present = m.store.files_present(model);
                let total = model.files.len();
                let on_disk = m.on_disk(model);
                let kept: u64 = on_disk.iter().map(|(b, _)| b).sum();
                let bytes: u64 = model.files.iter().map(|f| f.size).sum();
                let has_partial = on_disk.iter().any(|(b, complete)| !complete && *b > 0);
                let state = match (&busy, &job) {
                    (Some(id), Some((state, _))) if *id == model.id => state.clone(),
                    (Some(_), Some((_, ahead))) if ahead.contains(&model.id) => ModelState::Queued,
                    _ => match checked.get(&model.id) {
                        Some(Err(e)) => ModelState::Failed(e.clone()),
                        _ if has_partial => ModelState::Paused {
                            done: kept,
                            total: bytes,
                        },
                        _ if present == 0 => ModelState::NotDownloaded,
                        _ if present < total => ModelState::Partial { present, total },
                        Some(Ok(())) => ModelState::Verified,
                        None => ModelState::Downloaded,
                    },
                };
                let uses: Vec<&str> = in_use
                    .iter()
                    .filter(|(id, _)| *id == model.id)
                    .map(|(_, label)| label.as_str())
                    .collect();
                ModelRow {
                    id: model.id.clone(),
                    role: match model.role {
                        Role::Vlm => "vision",
                        Role::Stt => "speech-to-text",
                        Role::Tts => "voice",
                    }
                    .to_owned(),
                    license: model.license.clone(),
                    repo: model.repo.clone(),
                    revision: model.revision.clone(),
                    bytes,
                    bytes_missing: m.missing_bytes(model),
                    state,
                    in_use: (!uses.is_empty()).then(|| uses.join(", ")),
                }
            })
            .collect()
    }

    fn free_bytes(&self) -> Option<u64> {
        free_bytes(self.0.store.root())
    }

    fn download_many(&self, ids: &[String]) -> Result<(), DownloadIssue> {
        self.0.start_download(ids)
    }

    fn progress(&self) -> Option<DownloadProgress> {
        lock(&self.0.progress).clone()
    }

    fn verify(&self, id: &str) -> Result<(), String> {
        let m = &self.0;
        let model = m.model(id)?.clone();
        if m.store.files_present(&model) < model.files.len() {
            return Err(format!("{id} is not fully downloaded"));
        }
        if let Some(other) = m.busy() {
            return Err(DownloadIssue::Busy(other).plain());
        }
        let files = model.files.len();
        let state = Arc::new(Mutex::new(ModelState::Verifying { file: 1, files }));
        let cancel = Arc::new(AtomicBool::new(false));
        let (task_state, task_cancel, task_m) =
            (Arc::clone(&state), Arc::clone(&cancel), Arc::clone(m));
        let task = m.runtime.spawn_blocking(move || {
            for (i, file) in model.files.iter().enumerate() {
                if task_cancel.load(Ordering::Relaxed) {
                    return;
                }
                *lock(&task_state) = ModelState::Verifying { file: i + 1, files };
                if let Err(e) = task_m.store.verify_file(&model, file) {
                    task_m.finish(&model.id, Err(e.to_string()));
                    return;
                }
            }
            task_m.finish(&model.id, Ok(()));
        });
        *lock(&m.job) = Some(Job {
            kind: JobKind::Verify,
            queue: vec![id.to_owned()],
            current: Arc::new(Mutex::new(id.to_owned())),
            state,
            cancel,
            task,
        });
        Ok(())
    }

    fn pause(&self) {
        self.0.stop(true);
    }

    fn resume(&self, id: Option<&str>) -> Result<(), DownloadIssue> {
        let queue = match id {
            Some(id) => vec![id.to_owned()],
            None => lock(&self.0.paused).clone(),
        };
        if queue.is_empty() {
            return Ok(());
        }
        self.0.start_download(&queue)
    }

    fn cancel(&self) {
        self.0.stop(false);
    }

    fn delete(&self, id: &str) -> Result<u64, String> {
        let m = &self.0;
        let model = m.model(id)?;
        if m.busy().as_deref() == Some(id) {
            return Err(format!(
                "{id} is being downloaded or checked; cancel that first"
            ));
        }
        if let Some((_, label)) = (m.in_use)().into_iter().find(|(used, _)| used == id) {
            return Err(format!(
                "{id} is in use ({label}); choose another model in Settings first"
            ));
        }
        let freed = m
            .store
            .remove(model)
            .map_err(|e| format!("could not delete {id}: {e}"))?;
        lock(&m.checked).remove(id);
        eprintln!(
            "glance: models: deleted {id} ({:.1} GB)",
            freed as f64 / 1e9
        );
        Ok(freed)
    }

    fn partials(&self) -> Vec<PartialRow> {
        let m = &self.0;
        let owner = |sha: &str| {
            m.manifest.models().iter().find_map(|model| {
                model
                    .files
                    .iter()
                    .find(|f| f.sha256 == sha)
                    .map(|f| (model.id.clone(), f.path.clone()))
            })
        };
        m.store
            .partials()
            .into_iter()
            .map(|p| PartialRow {
                belongs_to: p.sha256.as_deref().and_then(owner),
                name: p.name,
                bytes: p.bytes,
            })
            .collect()
    }

    fn clear_partials(&self) -> u64 {
        let m = &self.0;
        // Never the running download's files.
        let running: Vec<String> = match (m.busy(), lock(&m.job).as_ref()) {
            (Some(_), Some(job)) if job.kind == JobKind::Download => job
                .queue
                .iter()
                .filter_map(|id| m.manifest.model(id))
                .flat_map(|model| model.files.iter().map(|f| f.sha256.clone()))
                .collect(),
            _ => Vec::new(),
        };
        let keep: Vec<&str> = running.iter().map(String::as_str).collect();
        let freed = m.store.clear_partials(&keep);
        lock(&m.paused).clear();
        eprintln!(
            "glance: models: cleared unfinished downloads ({:.1} GB)",
            freed as f64 / 1e9
        );
        freed
    }
}

/// Free bytes on the volume holding `path` (or its nearest existing
/// parent), from `/bin/df`.
pub fn free_bytes(path: &Path) -> Option<u64> {
    let existing = path.ancestors().find(|p| p.exists())?;
    let out = std::process::Command::new("/bin/df")
        .arg("-k")
        .arg(existing)
        .output()
        .ok()?;
    parse_df(&String::from_utf8_lossy(&out.stdout))
}

/// The "Available" column (1K blocks) of `df -k` output.
fn parse_df(text: &str) -> Option<u64> {
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next()?.split_whitespace().collect();
    let column = header.iter().position(|h| h.starts_with("Avail"))?;
    let row: Vec<&str> = lines.next()?.split_whitespace().collect();
    row.get(column)?.parse::<u64>().ok().map(|k| k * 1024)
}

/// What the configuration uses, read fresh each time (so a change made in
/// Settings counts), plus what this instance started with (still loaded
/// until it restarts).
pub fn in_use(stt: String, at_start: crate::config::Config) -> InUse {
    let uses = |config: &crate::config::Config| {
        let mut list = Vec::new();
        let m = &config.models;
        if m.fast_endpoint.is_none() {
            list.push((m.fast.clone(), "FAST model".to_owned()));
        }
        if let (None, Some(deep)) = (&m.deep_endpoint, &m.deep) {
            list.push((deep.clone(), "DEEP model".to_owned()));
        }
        if config.speech.engine == crate::config::Engine::Kokoro {
            let name = config.speech.kokoro_model.as_str();
            let id = super::super::KOKORO_MODELS
                .iter()
                .find(|(short, _)| *short == name)
                .map_or(name, |(_, id)| id);
            list.push((id.to_owned(), "voice".to_owned()));
            list.push((super::super::KOKORO_VOICES.to_owned(), "voices".to_owned()));
        }
        list
    };
    let started = uses(&at_start);
    Box::new(move || {
        let mut list = vec![(stt.clone(), "speech recognition".to_owned())];
        let now = crate::config::Config::load()
            .map(|c| uses(&c))
            .unwrap_or_default();
        for entry in started.iter().chain(&now) {
            if !list.iter().any(|(id, _)| *id == entry.0) {
                list.push(entry.clone());
            }
        }
        list
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ABC_SHA: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn manifest() -> Manifest {
        let model = |id: &str| {
            format!(
                "[[model]]\nid = \"{id}\"\nrole = \"stt\"\nlicense = \"MIT\"\nrepo = \"o/r\"\n\
                 revision = \"0123456789abcdef0123456789abcdef01234567\"\n[[model.file]]\n\
                 path = \"a.onnx\"\nkind = \"weights\"\nformat = \"onnx\"\nsize = 3\nsha256 = \"{ABC_SHA}\"\n"
            )
        };
        Manifest::parse(&format!("schema = 1\n{}{}", model("used"), model("spare"))).unwrap()
    }

    fn setup(name: &str) -> (Models, ModelStore, tokio::runtime::Runtime) {
        let dir = std::env::temp_dir().join(format!("glance-panel-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = ModelStore::new(&dir);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let in_use: InUse = Box::new(|| vec![("used".to_owned(), "FAST model".to_owned())]);
        let manager =
            ModelsManager::new(manifest(), store.clone(), runtime.handle().clone(), in_use);
        (Models(manager), store, runtime)
    }

    fn put(store: &ModelStore, id: &str, bytes: &[u8]) {
        let path = store.root().join(id).join("a.onnx");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn state(models: &Models, id: &str) -> ModelState {
        models
            .rows()
            .into_iter()
            .find(|r| r.id == id)
            .unwrap()
            .state
    }

    fn wait_idle(models: &Models) {
        for _ in 0..200 {
            if models.0.busy().is_none() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("job did not finish");
    }

    #[test]
    fn rows_show_presence_and_use() {
        let (models, store, _rt) = setup("rows");
        put(&store, "used", b"abc");
        let rows = models.rows();
        assert_eq!(rows[0].state, ModelState::Downloaded);
        assert_eq!(rows[0].in_use.as_deref(), Some("FAST model"));
        assert_eq!(rows[0].bytes_missing, 0);
        assert_eq!(rows[1].state, ModelState::NotDownloaded);
        assert_eq!(rows[1].bytes_missing, 3);
        assert_eq!(rows[1].in_use, None);
    }

    #[test]
    fn verify_reports_good_and_bad_files() {
        let (models, store, _rt) = setup("verify");
        put(&store, "used", b"abc");
        put(&store, "spare", b"abd"); // right size, wrong bytes
        models.verify("used").unwrap();
        wait_idle(&models);
        assert_eq!(state(&models, "used"), ModelState::Verified);
        models.verify("spare").unwrap();
        wait_idle(&models);
        assert!(matches!(state(&models, "spare"), ModelState::Failed(e) if e.contains("SHA-256")));
    }

    #[test]
    fn delete_refuses_models_in_use_and_unknown_ids() {
        let (models, store, _rt) = setup("delete");
        put(&store, "used", b"abc");
        put(&store, "spare", b"abc");
        assert!(models.delete("used").unwrap_err().contains("in use"));
        assert!(
            models
                .delete("../etc")
                .unwrap_err()
                .contains("not in models.toml")
        );
        assert_eq!(
            models.download("nope"),
            Err(DownloadIssue::NotPinned("nope".into()).plain())
        );
        assert_eq!(models.delete("spare").unwrap(), 3);
        assert_eq!(state(&models, "spare"), ModelState::NotDownloaded);
        assert_eq!(state(&models, "used"), ModelState::Downloaded);
    }

    #[test]
    fn a_kept_partial_shows_as_paused_and_can_be_cleared() {
        let (models, store, _rt) = setup("partials");
        let manifest = manifest();
        let file = &manifest.model("spare").unwrap().files[0];
        std::fs::create_dir_all(store.partial_dir()).unwrap();
        std::fs::write(store.partial_path(file), b"ab").unwrap();
        std::fs::write(
            store.partial_dir().join(format!("{}.part", "c".repeat(64))),
            b"x",
        )
        .unwrap();
        // Both models pin the same hash in this test manifest, so both show it.
        assert_eq!(
            state(&models, "spare"),
            ModelState::Paused { done: 2, total: 3 }
        );
        let partials = models.partials();
        assert_eq!(partials.len(), 2);
        assert!(
            partials
                .iter()
                .any(|p| p.belongs_to.is_none() && p.bytes == 1)
        );
        assert!(
            partials
                .iter()
                .any(|p| p.belongs_to.is_some() && p.bytes == 2)
        );
        assert_eq!(models.clear_partials(), 3);
        assert!(models.partials().is_empty());
        assert_eq!(state(&models, "spare"), ModelState::NotDownloaded);
    }

    #[test]
    fn a_download_that_needs_more_space_than_the_disk_has_is_refused() {
        // The real disk is checked, so ask for a model larger than any disk.
        let big = Manifest::parse(&format!(
            "schema = 1\n[[model]]\nid = \"huge\"\nrole = \"stt\"\nlicense = \"MIT\"\nrepo = \"o/r\"\n\
             revision = \"0123456789abcdef0123456789abcdef01234567\"\n[[model.file]]\n\
             path = \"a.onnx\"\nkind = \"weights\"\nformat = \"onnx\"\nsize = {}\nsha256 = \"{ABC_SHA}\"\n",
            u64::MAX / 4
        ))
        .unwrap();
        let dir = std::env::temp_dir().join(format!("glance-panel-huge-{}", std::process::id()));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let models = Models(ModelsManager::new(
            big,
            ModelStore::new(&dir),
            runtime.handle().clone(),
            Box::new(Vec::new),
        ));
        assert!(matches!(
            models.download_many(&["huge".into()]),
            Err(DownloadIssue::NotEnoughSpace { .. })
        ));
        assert!(models.progress().is_none());
    }

    #[test]
    fn speed_and_time_left_come_from_the_last_seconds() {
        let t = Instant::now();
        let mut rate = Rate::default();
        rate.push(t, 0);
        assert_eq!(rate.per_sec(), None);
        rate.push(t + Duration::from_secs(1), 10_000_000);
        rate.push(t + Duration::from_secs(2), 20_000_000);
        assert_eq!(rate.per_sec(), Some(10_000_000.0));
        assert_eq!(
            remaining(20_000_000, 50_000_000, rate.per_sec()),
            Some(Duration::from_secs(3))
        );
        // Old samples fall out of the window.
        rate.push(t + Duration::from_secs(20), 60_000_000);
        rate.push(t + Duration::from_secs(21), 61_000_000);
        assert_eq!(rate.per_sec(), Some(1_000_000.0));
        // A file starting over resets the measurement.
        rate.push(t + Duration::from_secs(22), 5);
        assert_eq!(rate.per_sec(), None);
        assert_eq!(remaining(1, 2, None), None);
    }

    #[test]
    fn df_output_is_read() {
        let text = "Filesystem 1024-blocks      Used Available Capacity iused ifree %iused  Mounted on\n\
                    /dev/disk3s5 971350180 512000000 400000000    57% 1 2 0%   /System/Volumes/Data\n";
        assert_eq!(parse_df(text), Some(400_000_000 * 1024));
        assert_eq!(parse_df("garbage"), None);
        assert!(free_bytes(Path::new("/")).is_some_and(|b| b > 0));
    }
}
