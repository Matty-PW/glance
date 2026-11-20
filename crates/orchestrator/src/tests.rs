//! The state machine driven end to end with mocks. Real time, short
//! intervals: `min_press` is 100 ms here and holds are 150–200 ms.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use glance_audio::{AudioClip, AudioError, AudioInput, Recording};
use glance_capture::{CaptureError, ScreenCapture, Screenshot};
use glance_llm_client::{
    ChatError, ChatEvent, ChatModel, ChatRequest, ChatStream, ChatSummary, Part, Role, Timings,
};
use glance_stt::{SpeechToText, SttError, Transcript};
use glance_tts::{SpeechEvent, TextToSpeech, TtsError, UtteranceId};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::{
    Chord, Controller, DeepStatus, DeepTier, HotkeyEvent, LoadFuture, Orchestrator, Outcome, Parts,
    Progress, SessionInfo, TurnConfig, TurnError, TurnReport, TurnState, Update,
};

// ---- mocks -----------------------------------------------------------------

struct MockCapture {
    fail: Option<fn() -> CaptureError>,
    count: Arc<AtomicUsize>,
}

impl ScreenCapture for MockCapture {
    /// Every capture has different pixels, so requests can tell them apart.
    fn capture(&self) -> Result<Screenshot, CaptureError> {
        let n = self.count.fetch_add(1, Ordering::SeqCst) + 1;
        match self.fail {
            Some(error) => Err(error()),
            None => Screenshot::from_rgba(4, 4, vec![(n * 10) as u8; 4 * 4 * 4]),
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

/// Hands out `texts` in order, repeating the last one.
struct MockStt {
    texts: Vec<&'static str>,
    calls: Arc<AtomicUsize>,
}

impl SpeechToText for MockStt {
    fn transcribe(&mut self, clip: &AudioClip) -> Result<Transcript, SttError> {
        assert_eq!(clip.sample_rate(), glance_stt::SAMPLE_RATE);
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let text = self.texts[n.min(self.texts.len() - 1)];
        Ok(Transcript { text: text.into() })
    }
}

/// What a request looked like, without keeping image bytes around.
#[derive(Debug, Clone, PartialEq)]
struct SeenRequest {
    max_tokens: Option<u32>,
    /// The whole system prompt (compared by the personality tests only).
    system_text: String,
    /// Which system prompt: "plain", "escalation" or "deep".
    system: &'static str,
    /// `add_generation_prompt`, when the request sets it.
    generation_prompt: Option<bool>,
    /// Every message after the system prompt, as `U:part+part` or `A:text`.
    /// Images are `img1`, `img2`… in the order the mock first saw them;
    /// the placeholder for a replaced screenshot is `earlier`.
    layout: Vec<String>,
}

struct MockChat {
    reply: Vec<ChatEvent>,
    seen: Arc<Mutex<Vec<SeenRequest>>>,
    images: Mutex<Vec<Arc<[u8]>>>,
    fail_question: bool,
}

impl MockChat {
    fn label(&self, png: &Arc<[u8]>) -> String {
        let mut images = self.images.lock().unwrap();
        let index = match images.iter().position(|known| known == png) {
            Some(i) => i,
            None => {
                images.push(Arc::clone(png));
                images.len() - 1
            }
        };
        format!("img{}", index + 1)
    }
}

impl ChatModel for MockChat {
    fn chat(
        &self,
        request: ChatRequest,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<ChatStream, ChatError>> + Send + '_>> {
        let layout = request
            .messages
            .iter()
            .skip(1)
            .map(|m| {
                let parts: Vec<String> = m
                    .parts
                    .iter()
                    .map(|p| match p {
                        Part::Png(png) => self.label(png),
                        Part::Text(t) if t == crate::session::EARLIER_SCREENSHOT => {
                            "earlier".to_owned()
                        }
                        Part::Text(t) => t.clone(),
                    })
                    .collect();
                let role = if m.role == Role::User { "U" } else { "A" };
                format!("{role}:{}", parts.join("+"))
            })
            .collect();
        let prewarm = request.max_tokens == Some(1);
        let system = match request.messages.first().and_then(|m| m.parts.first()) {
            // A length hint may follow the prompt.
            Some(Part::Text(t)) if t.starts_with(crate::ESCALATION_SYSTEM_PROMPT) => "escalation",
            Some(Part::Text(t)) if t.starts_with(crate::DEEP_SYSTEM_PROMPT) => "deep",
            Some(Part::Text(t)) if t.starts_with(crate::SPOKEN_SYSTEM_PROMPT) => "plain",
            _ => "other",
        };
        let generation_prompt = request
            .extra_body
            .get("add_generation_prompt")
            .and_then(serde_json::Value::as_bool);
        let system_text = match request.messages.first().and_then(|m| m.parts.first()) {
            Some(Part::Text(t)) => t.clone(),
            _ => String::new(),
        };
        self.seen.lock().unwrap().push(SeenRequest {
            max_tokens: request.max_tokens,
            system_text,
            system,
            generation_prompt,
            layout,
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

/// DEEP on demand: counts loads and unloads; loading takes `load_delay`.
struct MockDeep {
    chat: Arc<MockChat>,
    loaded: std::sync::atomic::AtomicBool,
    loads: AtomicUsize,
    unloads: AtomicUsize,
    load_delay: Duration,
    /// One load at a time, as the app's OnDemandDeep does.
    loading: tokio::sync::Mutex<()>,
}

impl DeepTier for MockDeep {
    fn loaded(&self) -> Option<Arc<dyn ChatModel>> {
        self.loaded
            .load(Ordering::SeqCst)
            .then(|| Arc::clone(&self.chat) as Arc<dyn ChatModel>)
    }

    fn load(&self) -> LoadFuture<'_> {
        Box::pin(async move {
            let _one = self.loading.lock().await;
            if let Some(model) = self.loaded() {
                return Ok(model);
            }
            tokio::time::sleep(self.load_delay).await;
            self.loads.fetch_add(1, Ordering::SeqCst);
            self.loaded.store(true, Ordering::SeqCst);
            Ok(Arc::clone(&self.chat) as Arc<dyn ChatModel>)
        })
    }

    fn unload(&self) {
        self.unloads.fetch_add(1, Ordering::SeqCst);
        self.loaded.store(false, Ordering::SeqCst);
    }
}

// ---- harness ---------------------------------------------------------------

struct Setup {
    transcripts: Vec<&'static str>,
    /// DEEP's reply; `None` means no DEEP tier.
    deep: Option<Vec<ChatEvent>>,
    /// DEEP starts loaded (as with auto-escalation) or not (on demand).
    deep_loaded: bool,
    deep_load_delay: Duration,
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
            transcripts: vec!["what is this window"],
            deep: None,
            deep_loaded: false,
            deep_load_delay: Duration::from_millis(30),
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
    deep_requests: Arc<Mutex<Vec<SeenRequest>>>,
    deep: Option<Arc<MockDeep>>,
    tts: Arc<TtsLog>,
    captures: Arc<AtomicUsize>,
    controller: Controller,
    display: UnboundedReceiver<crate::Shown>,
}

impl Harness {
    fn new(setup: Setup) -> Self {
        let mic = Arc::new(MicLog::default());
        let stt_calls = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let deep_requests = Arc::new(Mutex::new(Vec::new()));
        let deep = setup.deep.map(|reply| {
            Arc::new(MockDeep {
                chat: Arc::new(MockChat {
                    reply,
                    seen: Arc::clone(&deep_requests),
                    images: Mutex::new(Vec::new()),
                    fail_question: false,
                }),
                loaded: std::sync::atomic::AtomicBool::new(setup.deep_loaded),
                loads: AtomicUsize::new(0),
                unloads: AtomicUsize::new(0),
                load_delay: setup.deep_load_delay,
                loading: tokio::sync::Mutex::new(()),
            })
        });
        let tts_log = Arc::new(TtsLog::default());
        let captures = Arc::new(AtomicUsize::new(0));
        let (events, speech_events) = mpsc::unbounded_channel();
        let (display_tx, display) = mpsc::unbounded_channel();
        let parts = Parts {
            capture: Arc::new(MockCapture {
                fail: setup.capture_fails,
                count: Arc::clone(&captures),
            }),
            mic: Box::new(MockMic {
                log: Arc::clone(&mic),
                silent: setup.silent_mic,
            }),
            stt: Box::new(MockStt {
                texts: setup.transcripts,
                calls: Arc::clone(&stt_calls),
            }),
            chat: Arc::new(MockChat {
                reply: setup.reply,
                seen: Arc::clone(&requests),
                images: Mutex::new(Vec::new()),
                fail_question: setup.chat_fails,
            }),
            deep: deep.clone().map(|d| d as Arc<dyn DeepTier>),
            tts: Arc::new(MockTts {
                events,
                next: AtomicU64::new(1),
                pending: Mutex::new(Vec::new()),
                hang: setup.tts_hangs,
                log: Arc::clone(&tts_log),
            }),
            speech_events,
            display: Some(display_tx),
        };
        let (orchestrator, updates) = Orchestrator::new(parts, setup.config);
        let controller = orchestrator.controller();
        let (keys, key_rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(orchestrator.run(key_rx));
        Self {
            keys: Some(keys),
            updates,
            task,
            mic,
            stt_calls,
            requests,
            deep_requests,
            deep,
            tts: tts_log,
            captures,
            controller,
            display,
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

    fn captures(&self) -> usize {
        self.captures.load(Ordering::SeqCst)
    }

    /// Hold `chord` long enough to count, then wait for the turn to end.
    async fn ask(&mut self, chord: Chord) -> Vec<Update> {
        self.hold(chord, 150).await;
        self.until_idle().await
    }

    fn requests(&self) -> Vec<SeenRequest> {
        self.requests.lock().unwrap().clone()
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
            _ => None,
        })
        .collect()
}

fn sessions(updates: &[Update]) -> Vec<SessionInfo> {
    updates
        .iter()
        .filter_map(|u| match u {
            Update::Session(info) => Some(*info),
            _ => None,
        })
        .collect()
}

fn reports(updates: &[Update]) -> Vec<TurnReport> {
    updates
        .iter()
        .filter_map(|u| match u {
            Update::Report(r) => Some((**r).clone()),
            _ => None,
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
    assert_eq!(
        h.requests(),
        [
            SeenRequest {
                max_tokens: Some(1),
                system_text: crate::SPOKEN_SYSTEM_PROMPT.into(),
                system: "plain",
                generation_prompt: None,
                layout: vec!["U:img1".into()],
            },
            SeenRequest {
                max_tokens: Some(200),
                system_text: crate::SPOKEN_SYSTEM_PROMPT.into(),
                system: "plain",
                generation_prompt: None,
                layout: vec!["U:img1+what is this window".into()],
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
    // The microphone opens on its worker thread just after Listening is
    // announced, so give it a moment under load.
    for _ in 0..100 {
        if h.mic.starts.load(Ordering::SeqCst) == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
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
        transcripts: vec![""],
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

// ---- sessions (M3) ---------------------------------------------------------

const ANSWER: &str = "It is a code editor. It shows a Rust file.";

fn layouts(requests: &[SeenRequest]) -> Vec<Vec<String>> {
    requests.iter().map(|r| r.layout.clone()).collect()
}

fn a(text: &str) -> String {
    format!("A:{text}")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn talk_follows_up_on_the_same_screenshot_without_capturing_again() {
    let mut h = Harness::new(Setup {
        transcripts: vec!["q1", "q2"],
        ..Default::default()
    });
    h.ask(Chord::Talk).await;
    let updates = h.ask(Chord::Talk).await;

    assert_eq!(h.captures(), 1);
    // No prewarm for the follow-up: the server already has the prefix.
    assert_eq!(
        layouts(&h.requests()),
        [
            vec!["U:img1".to_owned()],
            vec!["U:img1+q1".to_owned()],
            vec!["U:img1+q1".to_owned(), a(ANSWER), "U:q2".to_owned()],
        ]
    );
    let [report] = reports(&updates).try_into().unwrap();
    assert_eq!(report.outcome, Outcome::Answered);
    assert_eq!(report.fresh_image, Some(false));
    assert_eq!(report.history, Some(1));
    assert_eq!(report.capture, None);
    assert!(report.to_string().contains("image=reused history=1"));
    assert_eq!(sessions(&updates).last().unwrap().questions, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn look_captures_afresh_and_the_old_image_becomes_a_placeholder() {
    let mut h = Harness::new(Setup {
        transcripts: vec!["q1", "q2", "q3"],
        ..Default::default()
    });
    h.ask(Chord::Talk).await;
    h.ask(Chord::Look).await;
    h.ask(Chord::Talk).await;

    assert_eq!(h.captures(), 2);
    let requests = layouts(&h.requests());
    assert_eq!(requests.len(), 5, "{requests:?}");
    // The look's prewarm: history with the placeholder, then the new image.
    assert_eq!(
        requests[2],
        ["U:earlier+q1".to_owned(), a(ANSWER), "U:img2".to_owned()]
    );
    assert_eq!(
        requests[3],
        ["U:earlier+q1".to_owned(), a(ANSWER), "U:img2+q2".to_owned()]
    );
    // The follow-up after the look is about the new screenshot.
    assert_eq!(
        requests[4],
        [
            "U:earlier+q1".to_owned(),
            a(ANSWER),
            "U:img2+q2".to_owned(),
            a(ANSWER),
            "U:q3".to_owned()
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_look_leaves_the_session_as_it_was() {
    let mut h = Harness::new(Setup {
        transcripts: vec!["q1", "", "q3"],
        ..Default::default()
    });
    h.ask(Chord::Talk).await;
    let failed = h.ask(Chord::Look).await;
    assert_eq!(
        reports(&failed)[0].outcome,
        Outcome::Failed(TurnError::NoSpeech)
    );
    assert!(sessions(&failed).is_empty());
    h.ask(Chord::Talk).await;

    let last = h.questions().last().unwrap().layout.clone();
    assert_eq!(last, ["U:img1+q1".to_owned(), a(ANSWER), "U:q3".to_owned()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_session_expires_after_the_idle_timeout() {
    let mut h = Harness::new(Setup {
        transcripts: vec!["q1", "q2"],
        config: TurnConfig {
            min_press: Duration::from_millis(100),
            session_idle: Duration::from_millis(300),
            ..Default::default()
        },
        ..Default::default()
    });
    h.ask(Chord::Talk).await;
    let expired = h.until(|u| matches!(u, Update::Session(_))).await;
    assert_eq!(
        sessions(&expired),
        [SessionInfo {
            questions: 0,
            screenshot_taken: None
        }]
    );
    h.ask(Chord::Talk).await;
    assert_eq!(h.captures(), 2);
    assert_eq!(h.questions().last().unwrap().layout, ["U:img2+q2"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_session_cancels_the_turn_and_forgets_the_screenshot() {
    let mut h = Harness::new(Setup {
        transcripts: vec!["q1", "q2"],
        tts_hangs: true,
        ..Default::default()
    });
    h.hold(Chord::Talk, 150).await;
    h.until(|u| matches!(u, Update::State(TurnState::Speaking)))
        .await;

    h.controller.new_session();
    let updates = h.until(|u| matches!(u, Update::Session(_))).await;
    assert_eq!(reports(&updates)[0].outcome, Outcome::Cancelled);
    assert_eq!(sessions(&updates)[0].questions, 0);

    // The cancelled turn was never committed, so the next one captures.
    h.hold(Chord::Talk, 150).await;
    h.until(|u| matches!(u, Update::State(TurnState::Speaking)))
        .await;
    assert_eq!(h.captures(), 2);
    assert_eq!(h.questions().last().unwrap().layout, ["U:img2+q2"]);
}

// ---- escalation (M4) -------------------------------------------------------

fn signal(reason: &str) -> Vec<ChatEvent> {
    reply(&[
        "{\"glance\":\"escalate\",",
        &format!("\"v\":1,\"reason\":\"{reason}\"}}"),
    ])
}

/// Auto-escalation: DEEP loaded at start, FAST offered the escalate option.
fn with_deep(fast: Vec<ChatEvent>, deep: Vec<ChatEvent>) -> Setup {
    Setup {
        transcripts: vec!["what is the total", "and per month"],
        reply: fast,
        deep: Some(deep),
        deep_loaded: true,
        config: TurnConfig {
            min_press: Duration::from_millis(100),
            auto_escalate: true,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// DEEP on demand (the default): not loaded until "think harder".
fn on_demand(transcripts: Vec<&'static str>, deep: Vec<ChatEvent>) -> Setup {
    Setup {
        transcripts,
        reply: reply(&["It is a total row."]),
        deep: Some(deep),
        ..Default::default()
    }
}

/// DEEP's questions: everything but prewarms and prefills.
fn deep_questions(h: &Harness) -> Vec<SeenRequest> {
    h.deep_requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.max_tokens != Some(1))
        .cloned()
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_escalation_speaks_a_filler_then_deeps_answer() {
    let mut deep_reply = vec![ChatEvent::Reasoning("adding 3 and 4".into())];
    deep_reply.extend(reply(&["The total is seven. ", "That is all."]));
    let mut h = Harness::new(with_deep(signal("code_math"), deep_reply));
    let updates = h.ask(Chord::Talk).await;

    // FAST's signal is never spoken, nor DEEP's thinking.
    assert_eq!(
        h.spoken(),
        [
            "Let me work that out.",
            "The total is seven.",
            "That is all."
        ]
    );
    let [report] = reports(&updates).try_into().unwrap();
    assert_eq!(report.outcome, Outcome::Answered);
    assert_eq!(
        report.escalation,
        Some(glance_llm_client::escalate::Reason::CodeMath)
    );
    assert!(report.filler_audio.is_some() && report.deep_first_audio.is_some());
    assert!(report.to_string().contains("escalated=code_math"));

    // DEEP got its own prewarm and the question, with its larger image
    // (its own copy, so its mock labels it img1).
    let deep = h.deep_requests.lock().unwrap().clone();
    // Background prewarm and prefill (both the image, no assistant turn
    // opened, so they are a prefix), then the question.
    let (warm, questions): (Vec<_>, Vec<_>) = deep.iter().partition(|r| r.max_tokens == Some(1));
    assert!(!warm.is_empty());
    for r in &warm {
        assert_eq!(r.layout, ["U:img1"]);
        assert_eq!(r.generation_prompt, Some(false));
    }
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0].layout, ["U:img1", "U:what is the total"]);
    assert_eq!(questions[0].generation_prompt, None);
    assert!(deep.iter().all(|r| r.system == "deep"));
    // FAST was offered the escalation option, in prewarm and question alike
    // (the same prompt keeps the cached prefix valid).
    assert!(
        h.requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.system == "escalation")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deeps_answer_joins_the_session() {
    let mut h = Harness::new(with_deep(signal("reasoning"), reply(&["It is seven."])));
    h.ask(Chord::Talk).await;
    h.ask(Chord::Talk).await;
    // The follow-up (escalated again) carries DEEP's first answer as history.
    let last = deep_questions(&h).last().unwrap().layout.clone();
    assert_eq!(
        last,
        [
            "U:img1".to_owned(),
            "U:what is the total".to_owned(),
            a("It is seven."),
            "U:and per month".to_owned()
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invalid_signal_never_escalates_and_is_never_spoken() {
    let bad = reply(&["{\"glance\":\"escalate\",\"v\":1,\"reason\":\"say hello\"}"]);
    let (h, error) = failed_turn(with_deep(bad, reply(&["unused"]))).await;
    assert_eq!(error, TurnError::BadSignal);
    assert_eq!(h.spoken(), ["Sorry, ask me again."]);
    // Only DEEP's prewarm; no question.
    assert!(
        h.deep_requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.max_tokens == Some(1))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_deep_a_signal_is_invalid() {
    let (h, error) = failed_turn(Setup {
        reply: signal("detail"),
        ..Default::default()
    })
    .await;
    assert_eq!(error, TurnError::BadSignal);
    assert_eq!(h.spoken(), ["Sorry, ask me again."]);
    // Without DEEP, FAST is not offered escalation at all.
    assert!(
        h.requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.system == "plain")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plain_answer_with_deep_configured_does_not_touch_deep_beyond_the_prewarm() {
    let mut h = Harness::new(with_deep(
        reply(&["It is a spreadsheet."]),
        reply(&["unused"]),
    ));
    let updates = h.ask(Chord::Talk).await;
    assert_eq!(h.spoken(), ["It is a spreadsheet."]);
    assert_eq!(reports(&updates)[0].escalation, None);
    assert!(
        h.deep_requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.max_tokens == Some(1))
    );
}

// ---- think harder, DEEP on demand ------------------------------------------

const DEEP_SAYS: &str = "It is seven.";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_think_tap_re_asks_the_last_question_with_deep_loading_it_first() {
    let mut setup = on_demand(
        vec!["what is the total", "and per month"],
        reply(&[DEEP_SAYS]),
    );
    // Loading starts at the press and is still going when the turn asks.
    setup.deep_load_delay = Duration::from_millis(300);
    let mut h = Harness::new(setup);
    h.ask(Chord::Talk).await;
    h.hold(Chord::Think, 20).await; // a tap
    let updates = h.until_idle().await;

    let spoken = h.spoken();
    assert_eq!(
        spoken[1..],
        [
            "Let me think harder about that.",
            "Loading the bigger model first.",
            DEEP_SAYS
        ]
    );
    // DEEP gets the original question and screenshot, not FAST's answer.
    assert_eq!(
        deep_questions(&h)[0].layout,
        ["U:img1", "U:what is the total"]
    );
    assert_eq!(deep_questions(&h)[0].system, "deep");
    let deep = h.deep.as_ref().unwrap();
    assert_eq!(deep.loads.load(Ordering::SeqCst), 1);
    // FAST is never offered escalation when it is not opted in.
    assert!(h.requests().iter().all(|r| r.system == "plain"));

    let [report] = reports(&updates).try_into().unwrap();
    assert_eq!(report.outcome, Outcome::Answered);
    assert_eq!(report.think, Some("reask"));
    assert!(report.deep_load.is_some() && report.deep_prefill.is_some());
    assert!(report.to_string().contains("think=reask"));
    let progress: Vec<Progress> = updates
        .iter()
        .filter_map(|u| match u {
            Update::Progress(p) => Some(*p),
            _ => None,
        })
        .collect();
    assert_eq!(
        progress,
        [
            Progress::LoadingDeep,
            Progress::ReadingScreen,
            Progress::ThinkingHarder
        ]
    );
    assert!(
        updates
            .iter()
            .any(|u| matches!(u, Update::Deep(DeepStatus::Loaded)))
    );

    // DEEP's answer replaced FAST's in the session; no extra exchange.
    h.ask(Chord::Talk).await;
    assert_eq!(
        h.questions().last().unwrap().layout,
        [
            "U:img1+what is the total",
            "A:It is seven.",
            "U:and per month"
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn think_harder_with_more_words_is_a_new_question_on_the_same_screenshot() {
    let mut h = Harness::new(on_demand(
        vec!["what is the total", "Think harder: why is it slow?"],
        reply(&[DEEP_SAYS]),
    ));
    h.ask(Chord::Talk).await;
    let updates = h.ask(Chord::Talk).await;
    assert_eq!(h.captures(), 1);
    assert_eq!(
        deep_questions(&h)[0].layout,
        [
            "U:img1".to_owned(),
            "U:what is the total".to_owned(),
            a("It is a total row."),
            "U:why is it slow?".to_owned()
        ]
    );
    assert_eq!(reports(&updates)[0].think, Some("new"));
    // Only FAST's first question went to FAST.
    assert_eq!(h.questions().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saying_only_think_harder_re_asks() {
    let mut h = Harness::new(on_demand(
        vec!["what is the total", "Think harder."],
        reply(&[DEEP_SAYS]),
    ));
    h.ask(Chord::Talk).await;
    let updates = h.ask(Chord::Talk).await;
    assert_eq!(reports(&updates)[0].think, Some("reask"));
    assert_eq!(
        deep_questions(&h)[0].layout,
        ["U:img1", "U:what is the total"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn holding_the_think_chord_and_speaking_asks_deep_directly() {
    let mut h = Harness::new(on_demand(vec!["why is it slow"], reply(&[DEEP_SAYS])));
    let updates = h.ask(Chord::Think).await;
    assert_eq!(reports(&updates)[0].think, Some("new"));
    // No session yet, so it captured; FAST was not asked at all.
    assert_eq!(h.captures(), 1);
    assert!(h.questions().is_empty());
    assert_eq!(deep_questions(&h)[0].layout, ["U:img1", "U:why is it slow"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn think_harder_without_deep_says_how_to_set_it_up() {
    let mut h = Harness::new(Setup::default());
    h.hold(Chord::Think, 20).await;
    let updates = h.until_idle().await;
    assert_eq!(
        reports(&updates)[0].outcome,
        Outcome::Failed(TurnError::NoDeep)
    );
    assert!(h.spoken()[0].starts_with("Thinking harder isn't set up"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn think_harder_before_any_question_says_so() {
    let mut h = Harness::new(on_demand(vec!["unused"], reply(&[DEEP_SAYS])));
    h.hold(Chord::Think, 20).await;
    let updates = h.until_idle().await;
    assert_eq!(
        reports(&updates)[0].outcome,
        Outcome::Failed(TurnError::NothingToReask)
    );
    assert_eq!(h.deep.as_ref().unwrap().loads.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deep_unloads_after_the_idle_time_and_reloads_when_asked_again() {
    let mut setup = on_demand(
        vec!["what is the total", "Think harder."],
        reply(&[DEEP_SAYS]),
    );
    setup.config.deep_idle = Duration::from_millis(300);
    let mut h = Harness::new(setup);
    h.ask(Chord::Talk).await;
    h.hold(Chord::Think, 20).await;
    h.until_idle().await;
    h.until(|u| matches!(u, Update::Deep(DeepStatus::NotLoaded)))
        .await;
    let deep = h.deep.as_ref().unwrap();
    assert_eq!(deep.unloads.load(Ordering::SeqCst), 1);
    h.hold(Chord::Think, 20).await;
    h.until_idle().await;
    assert_eq!(h.deep.as_ref().unwrap().loads.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_silence_while_deep_works_gets_a_still_thinking() {
    let mut setup = on_demand(vec!["what is the total"], reply(&[DEEP_SAYS]));
    setup.deep_load_delay = Duration::from_millis(800);
    setup.config.deep_nudge = Duration::from_millis(100);
    let mut h = Harness::new(setup);
    h.ask(Chord::Talk).await;
    h.hold(Chord::Think, 20).await;
    h.until_idle().await;
    let spoken = h.spoken();
    let nudges = spoken.iter().filter(|s| *s == "Still thinking.").count();
    // At most twice, then only the tray and overlay show progress.
    assert!((1..=2).contains(&nudges), "{spoken:?}");
    assert_eq!(spoken.last().unwrap(), DEEP_SAYS);
}

// ---- response length --------------------------------------------------------

fn sentences(n: usize) -> Vec<ChatEvent> {
    let pieces: Vec<String> = (1..=n).map(|i| format!("Sentence number {i}. ")).collect();
    let refs: Vec<&str> = pieces.iter().map(String::as_str).collect();
    reply(&refs)
}

fn with_length(length: crate::ResponseLength, transcripts: Vec<&'static str>, n: usize) -> Setup {
    Setup {
        transcripts,
        reply: sentences(n),
        config: TurnConfig {
            min_press: Duration::from_millis(100),
            response_length: length,
            ..Default::default()
        },
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn short_stops_after_two_sentences_and_keeps_only_what_was_said() {
    use crate::ResponseLength::Short;
    let mut h = Harness::new(with_length(Short, vec!["what is this", "tell me more"], 5));
    let updates = h.ask(Chord::Talk).await;
    assert_eq!(h.spoken(), ["Sentence number 1.", "Sentence number 2."]);
    let report = &reports(&updates)[0];
    assert!(report.capped && report.to_string().contains("capped=1"));
    // The short hint is in the prompt.
    assert_eq!(h.questions()[0].system, "plain");

    // "Tell me more" continues from the spoken text.
    let updates = h.ask(Chord::Talk).await;
    assert!(reports(&updates)[0].more);
    assert_eq!(
        h.questions().last().unwrap().layout,
        [
            "U:img1+what is this".to_owned(),
            a("Sentence number 1. Sentence number 2."),
            format!("U:{}", crate::length::CONTINUE_PROMPT)
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn medium_has_only_a_safety_cap_and_long_has_none() {
    use crate::ResponseLength::{Long, Medium};
    let mut h = Harness::new(with_length(Medium, vec!["what is this"], 10));
    h.ask(Chord::Talk).await;
    assert_eq!(h.spoken().len(), crate::length::MEDIUM_CAP);
    assert_eq!(h.questions()[0].max_tokens, Some(200));

    let mut h = Harness::new(with_length(Long, vec!["what is this"], 10));
    h.ask(Chord::Talk).await;
    assert_eq!(h.spoken().len(), 10);
    assert_eq!(h.questions()[0].max_tokens, Some(600));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_explicit_walkthrough_lifts_the_cap_even_when_short() {
    use crate::ResponseLength::Short;
    let mut h = Harness::new(with_length(
        Short,
        vec!["Walk me through how to fix this, step by step."],
        5,
    ));
    let updates = h.ask(Chord::Talk).await;
    assert_eq!(h.spoken().len(), 5);
    assert!(!reports(&updates)[0].capped);
    assert_eq!(h.questions()[0].max_tokens, Some(600));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tell_me_more_after_deep_continues_with_deep() {
    let mut h = Harness::new(on_demand(
        vec!["what is the total", "Think harder.", "Go on."],
        reply(&[DEEP_SAYS]),
    ));
    h.ask(Chord::Talk).await;
    h.ask(Chord::Talk).await;
    let updates = h.ask(Chord::Talk).await;
    assert!(reports(&updates)[0].more);
    let last = deep_questions(&h).last().unwrap().layout.clone();
    assert_eq!(
        last,
        [
            "U:img1".to_owned(),
            "U:what is the total".to_owned(),
            a(DEEP_SAYS),
            format!("U:{}", crate::length::CONTINUE_PROMPT)
        ]
    );
    // FAST answered only the first question.
    assert_eq!(h.questions().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tell_me_more_with_nothing_to_continue_is_an_ordinary_question() {
    let mut h = Harness::new(Setup {
        transcripts: vec!["Tell me more."],
        ..Default::default()
    });
    let updates = h.ask(Chord::Talk).await;
    assert!(!reports(&updates)[0].more);
    assert_eq!(h.questions()[0].layout, ["U:img1+Tell me more."]);
}

// ---- overlay feed -----------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_overlay_gets_the_question_then_each_sentence_as_it_is_spoken() {
    use crate::Shown;
    let mut h = Harness::new(Setup::default());
    h.ask(Chord::Talk).await;
    let mut shown = Vec::new();
    while let Ok(s) = h.display.try_recv() {
        shown.push(s);
    }
    assert_eq!(
        shown,
        [
            Shown::Clear,
            Shown::Asked("what is this window".into()),
            Shown::Said("It is a code editor.".into()),
            Shown::Said("It shows a Rust file.".into()),
        ]
    );
    // Its Debug never shows the text.
    assert_eq!(format!("{:?}", shown[1]), "Asked(19 chars)");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_answer_length_can_change_while_running() {
    use crate::ResponseLength::Short;
    let mut h = Harness::new(with_length(
        crate::ResponseLength::Long,
        vec!["what is this"],
        5,
    ));
    h.ask(Chord::Talk).await;
    assert_eq!(h.spoken().len(), 5);
    h.controller.set_response_length(Short);
    h.ask(Chord::Talk).await;
    assert_eq!(h.spoken().len(), 5 + 2);
}

fn with_personality(personality: crate::Personality) -> Setup {
    Setup {
        transcripts: vec!["what is this", "and this", "think harder"],
        deep: Some(reply(&["Worked out."])),
        config: TurnConfig {
            min_press: Duration::from_millis(100),
            personality,
            ..Default::default()
        },
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn neutral_leaves_every_prompt_exactly_as_it_was() {
    let mut h = Harness::new(with_personality(crate::Personality::Neutral));
    h.ask(Chord::Talk).await;
    h.ask(Chord::Think).await;
    let fast = h.requests();
    let deep = h.deep_requests.lock().unwrap().clone();
    assert!(!fast.is_empty() && !deep.is_empty());
    assert!(
        fast.iter()
            .all(|r| r.system_text == crate::SPOKEN_SYSTEM_PROMPT)
    );
    assert!(
        deep.iter()
            .all(|r| r.system_text == crate::DEEP_SYSTEM_PROMPT)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_personality_is_one_layer_after_fast_and_deep_prompts() {
    let friendly = crate::Personality::Friendly;
    let layer = friendly.layer().unwrap();
    let mut h = Harness::new(with_personality(friendly));
    h.ask(Chord::Talk).await;
    h.ask(Chord::Think).await;
    let fast = h.requests();
    let deep = h.deep_requests.lock().unwrap().clone();
    // Prewarms and questions alike.
    assert!(fast.iter().any(|r| r.max_tokens == Some(1)));
    assert!(deep.iter().any(|r| r.max_tokens == Some(1)));
    for r in &fast {
        assert_eq!(
            r.system_text,
            format!("{}{layer}", crate::SPOKEN_SYSTEM_PROMPT)
        );
    }
    for r in &deep {
        assert_eq!(
            r.system_text,
            format!("{}{layer}", crate::DEEP_SYSTEM_PROMPT)
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_change_mid_turn_waits_for_the_next_press() {
    let direct = crate::Personality::Direct;
    let layer = direct.layer().unwrap();
    let mut h = Harness::new(with_personality(crate::Personality::Neutral));
    // Changed while the key is held: this turn keeps its prompt, so its
    // prewarm and question still share one prefix.
    h.key(HotkeyEvent::Pressed(Chord::Talk));
    tokio::time::sleep(Duration::from_millis(50)).await;
    h.controller.set_personality(direct);
    tokio::time::sleep(Duration::from_millis(100)).await;
    h.key(HotkeyEvent::Released(Chord::Talk));
    h.until_idle().await;
    let first = h.requests();
    assert!(first.len() >= 2);
    assert!(
        first
            .iter()
            .all(|r| r.system_text == crate::SPOKEN_SYSTEM_PROMPT)
    );
    // The next turn uses the new one throughout.
    h.ask(Chord::Look).await;
    let second: Vec<_> = h.requests().into_iter().skip(first.len()).collect();
    assert!(second.len() >= 2);
    let changed = format!("{}{layer}", crate::SPOKEN_SYSTEM_PROMPT);
    assert!(second.iter().all(|r| r.system_text == changed));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn personality_does_not_change_the_length_cap() {
    for personality in [crate::Personality::Neutral, crate::Personality::Strict] {
        let mut setup = with_length(crate::ResponseLength::Short, vec!["what is this"], 5);
        setup.config.personality = personality;
        let mut h = Harness::new(setup);
        h.ask(Chord::Talk).await;
        assert_eq!(h.spoken().len(), 2);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn maths_in_words_changes_only_what_is_spoken() {
    let mut setup = Setup {
        reply: reply(&["So 2 x 2 = 4. ", "Done."]),
        ..Default::default()
    };
    setup.config.min_press = Duration::from_millis(100);
    let mut h = Harness::new(setup);
    h.ask(Chord::Talk).await;
    assert_eq!(h.spoken(), ["So 2 x 2 = 4.", "Done."]);
    h.controller.set_math_as_words(true);
    h.ask(Chord::Talk).await;
    assert_eq!(&h.spoken()[2..], ["So 2 times 2 equals 4.", "Done."]);
    // The overlay shows the answer as written.
    let mut said = Vec::new();
    while let Ok(shown) = h.display.try_recv() {
        if let crate::Shown::Said(text) = shown {
            said.push(text);
        }
    }
    assert!(
        said.iter().filter(|t| *t == "So 2 x 2 = 4.").count() == 2,
        "{said:?}"
    );
}
