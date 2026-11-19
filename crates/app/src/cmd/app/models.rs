//! The models panel's backend: list, download, verify and delete the models
//! pinned in `models.toml`, through the same downloader and store as
//! `glance models` (HTTPS from huggingface.co, allowlisted CDN redirects,
//! pinned size and SHA-256 checked before a file enters the store). Nothing
//! outside the manifest can be downloaded or deleted from here.
//!
//! One job (a download or a check) runs at a time; the window polls
//! [`ModelsBackend::rows`] for progress.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use glance_models::{Manifest, Model, ModelStore, Role};
use glance_orchestrator::settings::{ModelRow, ModelState, ModelsBackend};

/// Kept free beyond a download's size, so a full disk never stops macOS.
const DISK_MARGIN: u64 = 2_000_000_000;

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
}

struct Job {
    id: String,
    state: Arc<Mutex<ModelState>>,
    cancel: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
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
        })
    }

    fn model(&self, id: &str) -> Result<&Model, String> {
        self.manifest
            .model(id)
            .ok_or_else(|| format!("{id} is not in models.toml"))
    }

    /// The running job, if it has not finished.
    fn busy(&self) -> Option<String> {
        let mut job = lock(&self.job);
        if job.as_ref().is_some_and(|j| j.task.is_finished()) {
            *job = None;
        }
        job.as_ref().map(|j| j.id.clone())
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

    fn start(
        self: &Arc<Self>,
        id: &str,
        first: ModelState,
        work: impl FnOnce(
            Arc<Self>,
            Arc<Mutex<ModelState>>,
            Arc<AtomicBool>,
        ) -> tokio::task::JoinHandle<()>,
    ) -> Result<(), String> {
        if let Some(other) = self.busy() {
            return Err(format!(
                "{other} is still being worked on; wait or cancel it"
            ));
        }
        let state = Arc::new(Mutex::new(first));
        let cancel = Arc::new(AtomicBool::new(false));
        let task = work(Arc::clone(self), Arc::clone(&state), Arc::clone(&cancel));
        *lock(&self.job) = Some(Job {
            id: id.to_owned(),
            state,
            cancel,
            task,
        });
        Ok(())
    }

    fn finish(&self, id: &str, result: Result<(), String>) {
        if let Err(e) = &result {
            eprintln!("glance: models: {id}: {e}");
        }
        lock(&self.checked).insert(id.to_owned(), result);
    }
}

/// The trait is implemented on the `Arc` because jobs keep a reference.
pub struct Models(pub Arc<ModelsManager>);

impl ModelsBackend for Models {
    fn rows(&self) -> Vec<ModelRow> {
        let m = &self.0;
        let in_use = (m.in_use)();
        let busy = m.busy();
        let job_state = lock(&m.job).as_ref().map(|j| lock(&j.state).clone());
        let checked = lock(&m.checked).clone();
        m.manifest
            .models()
            .iter()
            .map(|model| {
                let present = m.store.files_present(model);
                let total = model.files.len();
                let state = match (&busy, &job_state) {
                    (Some(id), Some(state)) if *id == model.id => state.clone(),
                    _ if present == 0 => match checked.get(&model.id) {
                        Some(Err(e)) => ModelState::Failed(e.clone()),
                        _ => ModelState::NotDownloaded,
                    },
                    _ => match checked.get(&model.id) {
                        Some(Err(e)) => ModelState::Failed(e.clone()),
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
                    bytes: model.files.iter().map(|f| f.size).sum(),
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

    fn download(&self, id: &str) -> Result<(), String> {
        let m = &self.0;
        let model = m.model(id)?;
        let missing = m.missing_bytes(model);
        if let Some(free) = self.free_bytes()
            && free < missing + DISK_MARGIN
        {
            return Err(format!(
                "not enough disk space: {id} needs {:.1} GB more and {:.1} GB is free \
                 (glance keeps {:.0} GB spare)",
                missing as f64 / 1e9,
                free as f64 / 1e9,
                DISK_MARGIN as f64 / 1e9
            ));
        }
        let model = model.clone();
        eprintln!(
            "glance: models: downloading {} from huggingface.co/{} at {}",
            model.id, model.repo, model.revision
        );
        m.start(id, ModelState::Preparing, |m, state, _cancel| {
            m.runtime.clone().spawn(async move {
                let mut done: HashMap<String, u64> = HashMap::new();
                let total: u64 = model.files.iter().map(|f| f.size).sum();
                let result = glance_models::pull(&m.store, &model, |file, bytes, _| {
                    done.insert(file.path.clone(), bytes);
                    // Files already verified count as done.
                    let finished: u64 = model
                        .files
                        .iter()
                        .map(|f| done.get(&f.path).copied().unwrap_or(0))
                        .sum();
                    let present_before = total.saturating_sub(missing);
                    *lock(&state) = ModelState::Downloading {
                        done: (present_before + finished).min(total),
                        total,
                    };
                })
                .await;
                m.finish(&model.id, result.map(|_| ()).map_err(|e| e.to_string()));
            })
        })
    }

    fn verify(&self, id: &str) -> Result<(), String> {
        let m = &self.0;
        let model = m.model(id)?.clone();
        if m.store.files_present(&model) < model.files.len() {
            return Err(format!("{id} is not fully downloaded"));
        }
        let files = model.files.len();
        m.start(
            id,
            ModelState::Verifying { file: 1, files },
            |m, state, cancel| {
                m.runtime.clone().spawn_blocking(move || {
                    for (i, file) in model.files.iter().enumerate() {
                        if cancel.load(Ordering::Relaxed) {
                            return;
                        }
                        *lock(&state) = ModelState::Verifying { file: i + 1, files };
                        if let Err(e) = m.store.verify_file(&model, file) {
                            m.finish(&model.id, Err(e.to_string()));
                            return;
                        }
                    }
                    m.finish(&model.id, Ok(()));
                })
            },
        )
    }

    fn cancel(&self) {
        let m = &self.0;
        let Some(job) = lock(&m.job).take() else {
            return;
        };
        job.cancel.store(true, Ordering::Relaxed);
        job.task.abort();
        if let Some(model) = m.manifest.model(&job.id) {
            let freed = m.store.remove_partials(model);
            eprintln!(
                "glance: models: {} cancelled ({:.1} GB of unfinished download removed)",
                job.id,
                freed as f64 / 1e9
            );
        }
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
}

/// Free bytes on the volume holding `path` (or its nearest existing
/// parent), from `/bin/df`.
fn free_bytes(path: &Path) -> Option<u64> {
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
        assert!(models.download("nope").is_err());
        assert_eq!(models.delete("spare").unwrap(), 3);
        assert_eq!(state(&models, "spare"), ModelState::NotDownloaded);
        assert_eq!(state(&models, "used"), ModelState::Downloaded);
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
