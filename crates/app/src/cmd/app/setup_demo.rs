//! `glance app --setup-demo`: the setup window on made-up data, to review
//! every step without touching real permissions, downloads, models or
//! config.toml. Also the fixed scenes `glance ui-snapshots` draws.
//!
//! The script: a 48 GB M4 Pro; Screen Recording off, then allowed but
//! needing a restart, then on (the window's demo permissions); the download
//! fails once at about 40 % of its second file ("couldn't reach
//! huggingface.co"), resumes on Try again, and finishes; the first test
//! question fails at speech-to-text, the second "works" (the overlay plays
//! a scripted turn; there is no voice in the demo).

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use glance_orchestrator::settings::{
    DownloadIssue, DownloadPhase, DownloadProgress, FileProgress, FileState,
};
use glance_orchestrator::setup::{
    MachineInfo, PresetInfo, RamTier, SetupBackend, SetupSnapshot, Skip, Stage, Step, TestResult,
};
use glance_orchestrator::{Shell, Shown, TurnState, Update};

/// Fake bytes per second: the whole demo download takes about 12 s.
const SPEED: f64 = 380_000_000.0;

const GB: u64 = 1_000_000_000;

/// (model, path, bytes) of the Basics download.
fn basics_files() -> Vec<(String, String, u64)> {
    let mut files = vec![
        (
            "qwen3-vl-4b-instruct-q4km",
            "Qwen3VL-4B-Instruct-Q4_K_M.gguf",
            2_497_281_664,
        ),
        (
            "qwen3-vl-4b-instruct-q4km",
            "mmproj-Qwen3VL-4B-Instruct-F16.gguf",
            836_180_256,
        ),
        (
            "parakeet-tdt-0.6b-v3-int8",
            "encoder-model.int8.onnx",
            652_183_999,
        ),
        (
            "parakeet-tdt-0.6b-v3-int8",
            "decoder_joint-model.int8.onnx",
            18_202_004,
        ),
        ("parakeet-tdt-0.6b-v3-int8", "vocab.txt", 93_939),
        ("kokoro-82m-v1.0-fp32", "onnx/model.onnx", 325_532_232),
        ("kokoro-82m-v1.0-fp32", "tokenizer.json", 3_497),
    ]
    .into_iter()
    .map(|(m, p, b)| (m.to_owned(), p.to_owned(), b))
    .collect::<Vec<_>>();
    for i in 0..28 {
        files.push((
            "kokoro-82m-v1.0-voices-en".into(),
            format!("voices/voice_{i:02}.bin"),
            522_240,
        ));
    }
    files
}

fn presets(tier: RamTier, missing: bool) -> Vec<PresetInfo> {
    let tested = tier == RamTier::Gb48;
    let note = if tested {
        "Measured on a 48 GB M4 Pro."
    } else {
        "Measured on a 48 GB M4 Pro; untested on a Mac like this one."
    };
    let speech = [
        "Speech-to-text: Parakeet v3",
        "Voice: Kokoro",
        "Voices: 28 English Kokoro voices",
    ];
    let make = |id: &str,
                name: &str,
                summary: &str,
                first: &[&str],
                total: u64,
                memory: &str,
                from: RamTier| PresetInfo {
        id: id.into(),
        name: name.into(),
        summary: summary.into(),
        includes: first
            .iter()
            .chain(&speech)
            .map(|s| (*s).to_owned())
            .collect(),
        total_bytes: total,
        missing_bytes: if missing { total } else { 0 },
        memory: memory.into(),
        memory_note: note.into(),
        offered: rank(from) <= rank(tier),
        fits_disk: true,
    };
    vec![
        make(
            "basics",
            "Basics",
            "One small vision model. Fast answers about what is on screen.",
            &["Vision: Qwen3-VL 4B"],
            4_344_000_000,
            "about 7.0 GB",
            RamTier::BelowMinimum,
        ),
        make(
            "sharper",
            "Sharper reading",
            "A larger vision model in place of the small one.",
            &["Vision: Qwen3-VL 8B"],
            7_197_000_000,
            "about 9.8 GB",
            RamTier::Gb32,
        ),
        make(
            "think",
            "Basics + think harder",
            "The small model, plus a large one that loads only when you ask it to think \
             harder, and unloads when idle.",
            &[
                "Vision: Qwen3-VL 4B",
                "Think harder: Qwen3.8 27B, loaded on demand",
            ],
            22_714_000_000,
            "about 7.0 GB; 26.4 GB while think harder is loaded",
            RamTier::Gb48,
        ),
    ]
}

fn rank(tier: RamTier) -> u8 {
    match tier {
        RamTier::BelowMinimum => 0,
        RamTier::Gb16 => 1,
        RamTier::Gb32 => 2,
        RamTier::Gb48 => 3,
    }
}

/// The fake download's state.
struct Fake {
    done: u64,
    phase: DownloadPhase,
    last: Instant,
    failed_once: bool,
    /// Bytes per second, as the window would see it.
    rate: Option<f64>,
}

struct State {
    chosen: String,
    ram: u64,
    download: Option<Fake>,
    test: TestResult,
    test_runs: u32,
    /// When the demo test's "thinking" ends.
    test_until: Option<Instant>,
    /// A fixed scene for snapshots.
    scene: Option<String>,
}

pub struct DemoSetup {
    state: Mutex<State>,
    shell: OnceLock<Arc<dyn Shell>>,
    runtime: tokio::runtime::Handle,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl DemoSetup {
    pub fn new(runtime: tokio::runtime::Handle) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                chosen: "basics".into(),
                ram: 48 << 30,
                download: None,
                test: TestResult::Idle,
                test_runs: 0,
                test_until: None,
                scene: None,
            }),
            shell: OnceLock::new(),
            runtime,
        })
    }

    /// The shell, to play the overlay during the demo test.
    pub fn attach(&self, shell: Arc<dyn Shell>) {
        let _ = self.shell.set(shell);
    }

    /// Snapshots: show the named scene (see `SCENES` in ui_snapshots).
    pub fn set_scene(&self, name: &str) {
        let mut s = lock(&self.state);
        s.scene = Some(name.to_owned());
        s.ram = if name == "your-mac-8gb" {
            8 << 30
        } else {
            48 << 30
        };
        s.chosen = "basics".into();
        let total: u64 = basics_files().iter().map(|f| f.2).sum();
        let fake = |done: u64, phase: DownloadPhase, rate: Option<f64>| Fake {
            done,
            phase,
            last: Instant::now(),
            failed_once: true,
            rate,
        };
        s.download = match name {
            "download-running" => Some(fake(
                total * 2 / 5,
                DownloadPhase::Running,
                Some(38_400_000.0),
            )),
            "download-failed" => Some(fake(
                2_830_000_000,
                DownloadPhase::Failed(DownloadIssue::Offline),
                None,
            )),
            "download-done" | "try-it" | "try-it-listening" | "try-it-works" | "try-it-failed"
            | "done" | "all-set" => Some(fake(total, DownloadPhase::Finished, None)),
            _ => None,
        };
        s.test = match name {
            "try-it-listening" => TestResult::Listening,
            "try-it-works" => TestResult::Works {
                first_word: Duration::from_millis(1230),
            },
            "try-it-failed" => TestResult::Failed {
                stage: Stage::SpeechToText,
                hint: glance_orchestrator::setup::stage_of(
                    &glance_orchestrator::TurnError::NoSpeech,
                )
                .1
                .to_owned(),
            },
            _ => TestResult::Idle,
        };
    }

    /// Move the fake download on.
    fn advance(state: &mut State) {
        let total: u64 = basics_files().iter().map(|f| f.2).sum();
        let Some(d) = state.download.as_mut() else {
            return;
        };
        let now = Instant::now();
        let dt = now.duration_since(d.last).as_secs_f64();
        d.last = now;
        if d.phase != DownloadPhase::Running || state.scene.is_some() {
            return;
        }
        let files = basics_files();
        // Fail once at 40 % of the second file.
        let fail_at = files[0].2 + files[1].2 * 2 / 5;
        let next = d.done + (SPEED * dt) as u64;
        if !d.failed_once && next >= fail_at {
            d.done = fail_at;
            d.failed_once = true;
            d.phase = DownloadPhase::Failed(DownloadIssue::Offline);
            d.rate = None;
            eprintln!("glance: setup demo: download code=offline (scripted)");
            return;
        }
        d.done = next.min(total);
        d.rate = Some(SPEED);
        if d.done == total {
            d.phase = DownloadPhase::Finished;
            d.rate = None;
        }
    }

    fn progress(d: &Fake) -> DownloadProgress {
        let mut left = d.done;
        let files: Vec<FileProgress> = basics_files()
            .into_iter()
            .map(|(model, path, size)| {
                let done = left.min(size);
                left -= done;
                let state = if done == size {
                    FileState::Done
                } else if done > 0 {
                    FileState::Downloading
                } else {
                    FileState::Waiting
                };
                FileProgress {
                    model,
                    path,
                    done,
                    total: size,
                    state,
                }
            })
            .collect();
        let total = files.iter().map(|f| f.total).sum();
        DownloadProgress {
            phase: d.phase.clone(),
            done: d.done,
            total,
            remaining: d
                .rate
                .map(|r| Duration::from_secs_f64(total.saturating_sub(d.done) as f64 / r)),
            bytes_per_sec: d.rate,
            files,
        }
    }

    /// The scripted overlay turn for the demo test (fixed text).
    fn play_overlay(&self, answer: bool) {
        let Some(shell) = self.shell.get().cloned() else {
            return;
        };
        self.runtime.spawn(async move {
            let pause = |ms| tokio::time::sleep(Duration::from_millis(ms));
            shell.update(&Update::State(TurnState::Transcribing));
            pause(400).await;
            if !answer {
                shell.update(&Update::State(TurnState::Idle));
                return;
            }
            shell.display(&Shown::Asked(
                glance_orchestrator::setup::TEST_QUESTION.into(),
            ));
            shell.update(&Update::State(TurnState::Thinking));
            pause(800).await;
            shell.update(&Update::State(TurnState::Speaking));
            shell.display(&Shown::Said(
                "Weekly active users rose from 1,240 to 1,720 over four weeks.".into(),
            ));
            pause(2400).await;
            shell.update(&Update::State(TurnState::Idle));
        });
    }
}

impl SetupBackend for DemoSetup {
    fn snapshot(&self) -> SetupSnapshot {
        let mut s = lock(&self.state);
        Self::advance(&mut s);
        if let Some(until) = s.test_until
            && Instant::now() >= until
        {
            s.test_until = None;
            s.test = if s.test_runs == 1 {
                TestResult::Failed {
                    stage: Stage::SpeechToText,
                    hint: glance_orchestrator::setup::stage_of(
                        &glance_orchestrator::TurnError::NoSpeech,
                    )
                    .1
                    .to_owned(),
                }
            } else {
                TestResult::Works {
                    first_word: Duration::from_millis(1230),
                }
            };
        }
        let tier = RamTier::of(s.ram);
        let ready = s
            .download
            .as_ref()
            .is_some_and(|d| d.phase == DownloadPhase::Finished);
        let mut presets = presets(tier, !ready);
        if s.scene.as_deref() == Some("all-set") || s.scene.as_deref() == Some("done") {
            for p in &mut presets {
                p.missing_bytes = 0;
            }
        }
        SetupSnapshot {
            machine: MachineInfo {
                chip: Some("Apple M4 Pro".into()),
                ram_bytes: Some(s.ram),
                free_bytes: Some(212 * GB),
                llama_server: Ok("/opt/homebrew/bin/llama-server".into()),
            },
            tier: Some(tier),
            presets,
            recommended: "basics".into(),
            chosen: s.chosen.clone(),
            models_ready: ready,
            download: s.download.as_ref().map(Self::progress),
            test: s.test.clone(),
            hotkeys: [
                "Ctrl+Opt+G".into(),
                "Ctrl+Opt+Shift+G".into(),
                "Ctrl+Opt+T".into(),
            ],
            voice: "Kokoro (af_heart)".into(),
            needs_restart: false,
            screen_asked: true,
        }
    }

    fn choose(&self, preset: &str) {
        lock(&self.state).chosen = preset.to_owned();
    }

    fn download(&self) -> Result<(), DownloadIssue> {
        let mut s = lock(&self.state);
        let failed_once = s.download.as_ref().is_some_and(|d| d.failed_once);
        let done = s.download.as_ref().map_or(0, |d| d.done);
        s.download = Some(Fake {
            done,
            phase: DownloadPhase::Running,
            last: Instant::now(),
            failed_once,
            rate: None,
        });
        Ok(())
    }

    fn pause(&self) {
        let mut s = lock(&self.state);
        Self::advance(&mut s);
        if let Some(d) = s.download.as_mut() {
            d.phase = DownloadPhase::Paused;
            d.rate = None;
        }
    }

    fn resume(&self) -> Result<(), DownloadIssue> {
        self.download()
    }

    fn cancel(&self) {
        let mut s = lock(&self.state);
        if let Some(d) = s.download.as_mut() {
            d.done = 0;
            d.phase = DownloadPhase::Cancelled;
            d.rate = None;
        }
    }

    fn start_test(&self) -> Result<(), String> {
        {
            let mut s = lock(&self.state);
            if s.test.is_running() {
                return Ok(());
            }
            s.test = TestResult::Listening;
            s.test_runs += 1;
        }
        if let Some(shell) = self.shell.get() {
            shell.display(&Shown::Clear);
            shell.update(&Update::State(TurnState::Listening));
            shell.update(&Update::Screenshot(
                glance_orchestrator::ScreenshotUse::Fresh { display: None },
            ));
        }
        super::DEMO_RUNNING.store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    fn stop_test(&self) {
        let answer = {
            let mut s = lock(&self.state);
            if s.test != TestResult::Listening {
                return;
            }
            s.test = TestResult::Working;
            s.test_until = Some(Instant::now() + Duration::from_millis(1600));
            s.test_runs > 1
        };
        super::DEMO_RUNNING.store(false, std::sync::atomic::Ordering::Relaxed);
        self.play_overlay(answer);
    }

    fn save_step(&self, step: Step) {
        eprintln!("glance: setup demo: step={}", step.as_str());
    }

    fn finish(&self) -> Result<(), String> {
        eprintln!("glance: setup demo: finished (nothing is saved in the demo)");
        Ok(())
    }

    fn skip(&self) -> Skip {
        let ready = lock(&self.state)
            .download
            .as_ref()
            .is_some_and(|d| d.phase == DownloadPhase::Finished);
        if ready {
            Skip::MarkComplete
        } else {
            Skip::CloseOnly
        }
    }

    fn restart(&self, step: Step) {
        eprintln!(
            "glance: setup demo: a real setup would restart glance here and come back to \
             step={}",
            step.as_str()
        );
    }

    fn note_screen_asked(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demo_download_fails_once_then_finishes() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let demo = DemoSetup::new(rt.handle().clone());
        assert!(demo.snapshot().download.is_none());
        demo.download().unwrap();
        // Pretend a lot of time passed.
        let phase = |demo: &DemoSetup| {
            {
                let mut s = lock(&demo.state);
                if let Some(d) = s.download.as_mut() {
                    d.last -= Duration::from_secs(60);
                }
            }
            demo.snapshot().download.unwrap().phase
        };
        assert_eq!(phase(&demo), DownloadPhase::Failed(DownloadIssue::Offline));
        let kept = demo.snapshot().download.unwrap().done;
        assert!(kept > 0);
        demo.resume().unwrap();
        assert!(
            demo.snapshot().download.unwrap().done >= kept,
            "resumes, not restarts"
        );
        assert_eq!(phase(&demo), DownloadPhase::Finished);
        assert!(demo.snapshot().models_ready);
        assert_eq!(demo.skip(), Skip::MarkComplete);
    }

    #[test]
    fn every_scene_is_consistent() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let demo = DemoSetup::new(rt.handle().clone());
        demo.set_scene("your-mac-8gb");
        let snap = demo.snapshot();
        assert_eq!(snap.tier, Some(RamTier::BelowMinimum));
        assert_eq!(
            snap.presets.iter().filter(|p| p.offered).count(),
            1,
            "only Basics under 16 GB"
        );
        demo.set_scene("download-failed");
        assert!(matches!(
            demo.snapshot().download.unwrap().phase,
            DownloadPhase::Failed(_)
        ));
        demo.set_scene("done");
        assert!(demo.snapshot().models_ready);
    }
}
