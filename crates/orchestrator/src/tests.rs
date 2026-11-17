//! The state machine driven end to end with mocks. Real time, short
//! intervals: `min_press` is 100 ms here and holds are 150–200 ms.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use glance_audio::{AudioClip, AudioError, AudioInput, Recording};
use glance_capture::{CaptureError, ScreenCapture, Screenshot};
use glance_llm_client::{
    ChatError, ChatEvent, ChatModel, ChatRequest, ChatStream, ChatSummary, Part, Timings,
};
use glance_stt::{SpeechToText, SttError, Transcript};
use glance_tts::{SpeechEvent, TextToSpeech, TtsError, UtteranceId};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::{
    Chord, HotkeyEvent, Orchestrator, Outcome, Parts, TurnConfig, TurnError, TurnReport, TurnState,
    Update,
};

// ---- mocks -----------------------------------------------------------------

struct MockCapture {
    fail: Option<fn() -> CaptureError>,
}

impl ScreenCapture for MockCapture {
    fn capture(&self) -> Result<Screenshot, CaptureError> {
        match self.fail {
            Some(error) => Err(error()),
            None => Screenshot::from_rgba(4, 4, vec![200; 4 * 4 * 4]),
        }
    }
}

#[derive(Default)]
struct MicLog {
    starts: AtomicUsize,
    stops: AtomicUsize,
}

struct MockMic {
    log: Arc<MicLog>,
    silent: bool,
}

impl AudioInput for MockMic {
    fn start(&mut self) -> Result<(), AudioError> {
        self.log.starts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn stop(&mut self) -> Result<Recording, AudioError> {
        self.log.stops.fetch_add(1, Ordering::SeqCst);
        let level = if self.silent { 0.0 } else { 0.1 };
        Ok(Recording {
            // 48 kHz, so the worker's resampling to 16 kHz is exercised.
            clip: AudioClip::new(vec![level; 4_800], 48_000),
            start_latency: Some(Duration::from_millis(5)),
        })
    }
}

struct MockStt {
    text: &'static str,
    calls: Arc<AtomicUsize>,
}

impl SpeechToText for MockStt {
    fn transcribe(&mut self, clip: &AudioClip) -> Result<Transcript, SttError> {
        assert_eq!(clip.sample_rate(), glance_stt::SAMPLE_RATE);
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Transcript {
            text: self.text.into(),
        })
    }
}

/// What a request looked like, without keeping image bytes around.
#[derive(Debug, Clone, PartialEq)]
struct SeenRequest {
    max_tokens: Option<u32>,
    /// Parts of the last (user) message: "image" or the text.
    user_parts: Vec<String>,
}

struct MockChat {
    reply: Vec<ChatEvent>,
    seen: Arc<Mutex<Vec<SeenRequest>>>,
    fail_question: bool,
}

impl ChatModel for MockChat {
    fn chat(
        &self,
        request: ChatRequest,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<ChatStream, ChatError>> + Send + '_>> {
        let user_parts = request
            .messages
            .last()
            .map(|m| {
                m.parts
                    .iter()
                    .map(|p| match p {
                        Part::Png(_) => "image".to_owned(),
                        Part::Text(t) => t.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let prewarm = request.max_tokens == Some(1);
        self.seen.lock().unwrap().push(SeenRequest {
            max_tokens: request.max_tokens,
            user_parts,
        });
        let events = if prewarm {
            vec![ChatEvent::Done(ChatSummary {
                timings: Some(Timings {
                    prompt_n: Some(665),
                    cache_n: Some(0),
                    ..Default::default()
                }),
                ..Default::default()
            })]
        } else {
            self.reply.clone()
        };
        let fail = self.fail_question && !prewarm;
        Box::pin(async move {
            if fail {
                return Err(ChatError::Status {
                    status: 503,
                    body: "unavailable".into(),
                });
            }
            let (tx, rx) = mpsc::channel(16);
            tokio::spawn(async move {
                for event in events {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    if tx.send(Ok(event)).await.is_err() {
                        return;
                    }
                }
            });
            Ok(ChatStream::from_channel(rx))
        })
    }
}

#[derive(Default)]
struct TtsLog {
    spoken: Mutex<Vec<String>>,
    stops: AtomicUsize,
}

/// Reports `Started` at once, and `Finished` too unless `hang` is set, in
/// which case sentences only end when `stop()` cancels them.
struct MockTts {
    events: UnboundedSender<SpeechEvent>,
    next: AtomicU64,
    pending: Mutex<Vec<UtteranceId>>,
    hang: bool,
    log: Arc<TtsLog>,
}

impl TextToSpeech for MockTts {
    fn speak(&self, text: &str) -> Result<UtteranceId, TtsError> {
        let id = UtteranceId(self.next.fetch_add(1, Ordering::SeqCst));
        self.log.spoken.lock().unwrap().push(text.to_owned());
        let _ = self.events.send(SpeechEvent::Started(id));
        if self.hang {
            self.pending.lock().unwrap().push(id);
        } else {
            let _ = self.events.send(SpeechEvent::Finished(id));
        }
        Ok(id)
    }

    fn stop(&self) {
        self.log.stops.fetch_add(1, Ordering::SeqCst);
        for id in self.pending.lock().unwrap().drain(..) {
            let _ = self.events.send(SpeechEvent::Cancelled(id));
        }
    }
}

// ---- harness ---------------------------------------------------------------

struct Setup {
    transcript: &'static str,
    reply: Vec<ChatEvent>,
    capture_fails: Option<fn() -> CaptureError>,
    silent_mic: bool,
    tts_hangs: bool,
    chat_fails: bool,
    config: TurnConfig,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            transcript: "what is this window",
            reply: reply(&["It is a code ", "editor. It shows ", "a Rust file."]),
            capture_fails: None,
            silent_mic: false,
            tts_hangs: false,
            chat_fails: false,
            config: TurnConfig {
                min_press: Duration::from_millis(100),
                ..Default::default()
            },
        }
    }
}

fn reply(pieces: &[&str]) -> Vec<ChatEvent> {
    let mut events: Vec<ChatEvent> = pieces
        .iter()
        .map(|p| ChatEvent::Content((*p).to_owned()))
        .collect();
    events.push(ChatEvent::Done(ChatSummary {
        timings: Some(Timings {
            prompt_n: Some(15),
            cache_n: Some(660),
            predicted_per_second: Some(72.0),
            ..Default::default()
        }),
        ..Default::default()
    }));
    events
}

struct Harness {
    keys: Option<UnboundedSender<HotkeyEvent>>,
    updates: UnboundedReceiver<Update>,
    task: JoinHandle<()>,
    mic: Arc<MicLog>,
    stt_calls: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<SeenRequest>>>,
    tts: Arc<TtsLog>,
}

impl Harness {
    fn new(setup: Setup) -> Self {
        let mic = Arc::new(MicLog::default());
        let stt_calls = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let tts_log = Arc::new(TtsLog::default());
        let (events, speech_events) = mpsc::unbounded_channel();
        let parts = Parts {
            capture: Arc::new(MockCapture {
                fail: setup.capture_fails,
            }),
            mic: Box::new(MockMic {
                log: Arc::clone(&mic),
                silent: setup.silent_mic,
            }),
            stt: Box::new(MockStt {
                text: setup.transcript,
                calls: Arc::clone(&stt_calls),
            }),
            chat: Arc::new(MockChat {
                reply: setup.reply,
                seen: Arc::clone(&requests),
                fail_question: setup.chat_fails,
            }),
            tts: Arc::new(MockTts {
                events,
                next: AtomicU64::new(1),
                pending: Mutex::new(Vec::new()),
                hang: setup.tts_hangs,
                log: Arc::clone(&tts_log),
            }),
            speech_events,
        };
        let (orchestrator, updates) = Orchestrator::new(parts, setup.config);
        let (keys, key_rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(orchestrator.run(key_rx));
        Self {
            keys: Some(keys),
            updates,
            task,
            mic,
            stt_calls,
            requests,
            tts: tts_log,
        }
    }

    fn key(&self, event: HotkeyEvent) {
        self.keys.as_ref().unwrap().send(event).unwrap();
    }

    async fn hold(&self, chord: Chord, for_ms: u64) {
        self.key(HotkeyEvent::Pressed(chord));
        tokio::time::sleep(Duration::from_millis(for_ms)).await;
        self.key(HotkeyEvent::Released(chord));
    }

    /// Collect updates until `stop` matches one (inclusive).
    async fn until(&mut self, stop: impl Fn(&Update) -> bool) -> Vec<Update> {
        let mut seen = Vec::new();
        let collect = async {
            while let Some(update) = self.updates.recv().await {
                let done = stop(&update);
                seen.push(update);
                if done {
                    return;
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(5), collect)
            .await
            .expect("timed out waiting for an update");
        seen
    }

    async fn until_idle(&mut self) -> Vec<Update> {
        self.until(|u| matches!(u, Update::State(TurnState::Idle)))
            .await
    }

    fn spoken(&self) -> Vec<String> {
        self.tts.spoken.lock().unwrap().clone()
    }

    fn questions(&self) -> Vec<SeenRequest> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.max_tokens != Some(1))
            .cloned()
            .collect()
    }
}

fn states(updates: &[Update]) -> Vec<TurnState> {
    updates
        .iter()
        .filter_map(|u| match u {
            Update::State(s) => Some(*s),
            Update::Report(_) => None,
        })
        .collect()
}

fn reports(updates: &[Update]) -> Vec<TurnReport> {
    updates
        .iter()
        .filter_map(|u| match u {
            Update::Report(r) => Some((**r).clone()),
            Update::State(_) => None,
        })
        .collect()
}

// ---- tests -----------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_held_chord_is_answered_aloud_sentence_by_sentence() {
    let mut h = Harness::new(Setup::default());
    h.hold(Chord::Talk, 150).await;
    let updates = h.until_idle().await;

    use TurnState::*;
    assert_eq!(
        states(&updates),
        [Listening, Transcribing, Thinking, Speaking, Idle]
    );
    assert_eq!(
        h.spoken(),
        ["It is a code editor.", "It shows a Rust file."]
    );

    // Image-only prewarm first, then image + transcript.
    let requests = h.requests.lock().unwrap().clone();
    assert_eq!(
        requests,
        [
            SeenRequest {
                max_tokens: Some(1),
                user_parts: vec!["image".into()],
            },
            SeenRequest {
                max_tokens: Some(200),
                user_parts: vec!["image".into(), "what is this window".into()],
            },
        ]
    );

    let [report] = reports(&updates).try_into().unwrap();
    assert_eq!(report.outcome, Outcome::Answered);
    assert_eq!(report.sentences, 2);
    assert!(report.ttfa.is_some() && report.ttft.is_some() && report.stt.is_some());
    assert!(report.held.unwrap() >= Duration::from_millis(150));
    assert_eq!(report.prewarm_prompt_n, Some(665));
    assert_eq!(report.cache_n, Some(660));
    assert_eq!(h.mic.starts.load(Ordering::SeqCst), 1);
    assert_eq!(h.mic.stops.load(Ordering::SeqCst), 1);

    // The log line carries timings only, never what was said.
    let line = report.to_string();
    assert!(line.contains("ttfa_ms="), "{line}");
    for secret in ["what is this", "editor", "Rust"] {
        assert!(!line.contains(secret), "{line}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_short_press_only_cancels() {
    let mut h = Harness::new(Setup::default());
    h.hold(Chord::Talk, 20).await;
    let updates = h.until_idle().await;

    assert_eq!(states(&updates), [TurnState::Listening, TurnState::Idle]);
    let [report] = reports(&updates).try_into().unwrap();
    assert_eq!(report.outcome, Outcome::ShortPress);
    assert_eq!(h.stt_calls.load(Ordering::SeqCst), 0);
    assert!(h.questions().is_empty());
    assert!(h.spoken().is_empty());
    // The microphone was still closed again.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(h.mic.stops.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_press_while_speaking_cancels_and_listens_again() {
    let mut h = Harness::new(Setup {
        tts_hangs: true,
        ..Default::default()
    });
    h.hold(Chord::Talk, 150).await;
    h.until(|u| matches!(u, Update::State(TurnState::Speaking)))
        .await;

    h.key(HotkeyEvent::Pressed(Chord::Look));
    let updates = h
        .until(|u| matches!(u, Update::State(TurnState::Listening)))
        .await;
    let [report] = reports(&updates).try_into().unwrap();
    assert_eq!(report.outcome, Outcome::Cancelled);
    assert_eq!(report.turn, 1);
    assert!(h.tts.stops.load(Ordering::SeqCst) >= 1);
    assert_eq!(h.mic.starts.load(Ordering::SeqCst), 2);

    // The new turn runs normally; the cancelled one's leftovers are ignored.
    tokio::time::sleep(Duration::from_millis(150)).await;
    h.key(HotkeyEvent::Released(Chord::Look));
    h.until(|u| matches!(u, Update::State(TurnState::Speaking)))
        .await;
    assert_eq!(h.questions().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn releasing_the_other_chord_does_not_end_listening() {
    let mut h = Harness::new(Setup::default());
    h.key(HotkeyEvent::Pressed(Chord::Talk));
    h.key(HotkeyEvent::Pressed(Chord::Look));
    h.key(HotkeyEvent::Released(Chord::Look));
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(h.stt_calls.load(Ordering::SeqCst), 0);
    h.key(HotkeyEvent::Released(Chord::Talk));
    let updates = h.until_idle().await;
    assert_eq!(reports(&updates)[0].outcome, Outcome::Answered);
    assert_eq!(h.mic.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recording_stops_at_the_cap_without_a_release() {
    let mut h = Harness::new(Setup {
        config: TurnConfig {
            min_press: Duration::from_millis(100),
            max_recording: Duration::from_millis(300),
            ..Default::default()
        },
        ..Default::default()
    });
    h.key(HotkeyEvent::Pressed(Chord::Talk));
    let updates = h.until_idle().await;
    let [report] = reports(&updates).try_into().unwrap();
    assert_eq!(report.outcome, Outcome::Answered);
    assert!(report.held.unwrap() >= Duration::from_millis(300));

    // The late release changes nothing.
    h.key(HotkeyEvent::Released(Chord::Talk));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(h.updates.try_recv().is_err());
}

async fn failed_turn(setup: Setup) -> (Harness, TurnError) {
    let mut h = Harness::new(setup);
    h.hold(Chord::Talk, 150).await;
    let updates = h.until_idle().await;
    let [report] = reports(&updates).try_into().unwrap();
    match report.outcome {
        Outcome::Failed(e) => (h, e),
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_speech_says_so_and_asks_nothing() {
    let (h, error) = failed_turn(Setup {
        transcript: "",
        ..Default::default()
    })
    .await;
    assert_eq!(error, TurnError::NoSpeech);
    assert!(h.questions().is_empty());
    assert_eq!(h.spoken(), ["Sorry, I didn't catch that."]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silent_microphone_points_at_the_permission() {
    let (h, error) = failed_turn(Setup {
        silent_mic: true,
        ..Default::default()
    })
    .await;
    assert_eq!(error, TurnError::SilentMic);
    assert_eq!(h.stt_calls.load(Ordering::SeqCst), 0);
    assert!(h.spoken()[0].contains("Microphone permission"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_denied_screen_capture_points_at_the_permission() {
    let (h, error) = failed_turn(Setup {
        capture_fails: Some(|| CaptureError::PermissionDenied),
        ..Default::default()
    })
    .await;
    assert_eq!(error, TurnError::ScreenPermission);
    assert!(h.requests.lock().unwrap().is_empty());
    assert!(h.spoken()[0].contains("Screen Recording"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreachable_model_is_reported_without_the_body() {
    let (h, error) = failed_turn(Setup {
        chat_fails: true,
        ..Default::default()
    })
    .await;
    assert_eq!(error, TurnError::Chat("HTTP 503".into()));
    assert_eq!(h.spoken(), ["Sorry, I couldn't reach the model."]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thinking_text_is_never_spoken() {
    let mut events = vec![ChatEvent::Reasoning("secret plan".into())];
    events.extend(reply(&["Hello there."]));
    let mut h = Harness::new(Setup {
        reply: events,
        ..Default::default()
    });
    h.hold(Chord::Talk, 150).await;
    h.until_idle().await;
    assert_eq!(h.spoken(), ["Hello there."]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_reply_is_a_failure() {
    let (_, error) = failed_turn(Setup {
        reply: reply(&[]),
        ..Default::default()
    })
    .await;
    assert_eq!(error, TurnError::EmptyReply);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutting_down_the_hotkeys_cancels_the_turn_and_returns() {
    let mut h = Harness::new(Setup::default());
    h.key(HotkeyEvent::Pressed(Chord::Talk));
    h.until(|u| matches!(u, Update::State(TurnState::Listening)))
        .await;
    h.keys = None;
    let updates = h.until_idle().await;
    assert_eq!(reports(&updates)[0].outcome, Outcome::Cancelled);
    tokio::time::timeout(Duration::from_secs(1), &mut h.task)
        .await
        .expect("run did not return")
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(h.mic.stops.load(Ordering::SeqCst), 1);
}
