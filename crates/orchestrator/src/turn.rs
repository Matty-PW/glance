//! What one turn does once it is running: the key-down preparation (capture,
//! encode, prewarm) and the key-up answer. A turn goes to FAST, or to DEEP
//! when the user asks it to "think harder" (by phrase or by the think
//! chord), or when FAST escalates (only with `auto_escalate`).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use glance_audio::{AudioError, Recording};
use glance_capture::{CaptureError, ScreenCapture};
use glance_llm_client::escalate::{Decision, EscalationGate, GRAMMAR, Reason, Step};
use glance_llm_client::{
    ChatError, ChatEvent, ChatModel, ChatRequest, ChatStream, ChatSummary, Message, Part, Timings,
};
use glance_tts::{ChunkerConfig, SentenceChunker, SpeechEvent, TextToSpeech, UtteranceId};
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::task::{JoinError, JoinHandle};
use tokio::time::Instant;

use crate::session::{Commit, Shot, Tier};
use crate::workers::{MicWorker, Reply, SttWorker};
use crate::{
    Chord, DeepStatus, DeepTier, Outcome, Parts, Progress, TurnConfig, TurnError, TurnReport,
    TurnState, Update,
};

/// Spoken when a "think harder" turn starts; local text only.
const THINK_CUE: &str = "Let me think harder about that.";
/// Spoken before DEEP is loaded.
const LOADING_CUE: &str = "Loading the bigger model first.";
/// Spoken while DEEP works and nothing else is being said.
const NUDGE_CUE: &str = "Still thinking.";

/// How long a fixed error line may take to be spoken before giving up.
const ERROR_LINE_TIMEOUT: Duration = Duration::from_secs(15);

/// Everything a turn needs, shared between the state machine and the tasks
/// it spawns.
pub(crate) struct Shared {
    pub(crate) capture: Arc<dyn ScreenCapture>,
    pub(crate) mic: MicWorker,
    pub(crate) stt: SttWorker,
    pub(crate) chat: Arc<dyn ChatModel>,
    /// The DEEP tier, if one is configured (loaded on demand).
    pub(crate) deep: Option<Arc<dyn DeepTier>>,
    /// For DEEP's load status, which is not tied to a turn.
    pub(crate) updates: UnboundedSender<Update>,
    pub(crate) tts: Arc<dyn TextToSpeech>,
    /// Held by whichever turn is speaking. Events carry utterance ids, so a
    /// turn ignores anything left over from a cancelled one.
    pub(crate) speech: Mutex<UnboundedReceiver<SpeechEvent>>,
    pub(crate) config: TurnConfig,
}

impl Shared {
    pub(crate) fn new(parts: Parts, config: TurnConfig, updates: UnboundedSender<Update>) -> Self {
        Self {
            capture: parts.capture,
            mic: MicWorker::spawn(parts.mic),
            stt: SttWorker::spawn(parts.stt),
            chat: parts.chat,
            deep: parts.deep,
            updates,
            tts: parts.tts,
            speech: Mutex::new(parts.speech_events),
            config,
        }
    }

    fn request(&self, tier: Tier, messages: Vec<Message>, max_tokens: u32) -> ChatRequest {
        let extra_body: Map<String, Value> = match tier {
            Tier::Fast => self.config.extra_body.clone(),
            Tier::Deep => self.config.deep_extra_body.clone(),
        };
        ChatRequest {
            messages,
            max_tokens: Some(max_tokens),
            temperature: Some(0.0),
            extra_body,
        }
    }

    /// FAST's prompt offers escalation only with `auto_escalate` and a DEEP
    /// tier (`docs/DESIGN.md` §7.3, rule 4). Prewarm and question must use
    /// the same one, or the cached prefix would not match.
    fn system(&self, tier: Tier) -> Message {
        let prompt = match (tier, self.escalates()) {
            (Tier::Deep, _) => &self.config.deep_system_prompt,
            (Tier::Fast, true) => &self.config.escalation_prompt,
            (Tier::Fast, false) => &self.config.system_prompt,
        };
        Message::system(prompt.clone())
    }

    fn escalates(&self) -> bool {
        self.config.auto_escalate && self.deep.is_some()
    }

    fn deep_status(&self, status: DeepStatus) {
        let _ = self.updates.send(Update::Deep(status));
    }

    /// The system prompt and `history`, with nothing after it.
    fn prefix(&self, tier: Tier, history: Vec<Message>) -> Vec<Message> {
        let mut messages = Vec::with_capacity(history.len() + 1);
        messages.push(self.system(tier));
        messages.extend(history);
        messages
    }

    /// The model list for `tier`: system prompt, history, then `last`.
    fn messages(&self, tier: Tier, history: Vec<Message>, last: Message) -> Vec<Message> {
        let mut messages = Vec::with_capacity(history.len() + 2);
        messages.push(self.system(tier));
        messages.extend(history);
        messages.push(last);
        messages
    }
}

/// A task handle that aborts the task when dropped, so cancelling a turn
/// cancels everything it started. Awaiting it yields the task's output.
pub(crate) struct AbortOnDrop<T>(pub(crate) JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl<T> Future for AbortOnDrop<T> {
    type Output = Result<T, JoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx)
    }
}

/// Messages from a turn's task back to the state machine, tagged with the
/// turn so that a cancelled turn cannot change the state.
pub(crate) enum Internal {
    State(u64, TurnState),
    Progress(u64, Progress),
    /// The turn is over. `Commit` is present only when it was answered.
    Done(Box<TurnReport>, Option<Commit>),
}

struct Notifier<'a> {
    turn: u64,
    internal: &'a UnboundedSender<Internal>,
}

impl Notifier<'_> {
    fn state(&self, state: TurnState) {
        let _ = self.internal.send(Internal::State(self.turn, state));
    }

    fn progress(&self, progress: Progress) {
        let _ = self.internal.send(Internal::Progress(self.turn, progress));
    }
}

/// Result of the key-down work. A follow-up captures nothing.
pub(crate) struct Prepared {
    shot: Option<Shot>,
    capture: Option<Duration>,
    encode: Option<Duration>,
    prewarm: Option<Prewarm>,
}

struct Prewarm {
    elapsed: Duration,
    timings: Option<Timings>,
}

/// Key-down: when the turn needs a screenshot, capture and encode the
/// screen, then send history + image without a question, so the server has
/// processed the image by the time the question arrives (`docs/DESIGN.md`
/// §4, "Prompt layout and prewarm"). A follow-up needs neither: its prefix
/// is already cached from the previous turn.
pub(crate) async fn prepare(
    shared: Arc<Shared>,
    history: Vec<Message>,
    deep_history: Vec<Message>,
    capture: bool,
    fast_prewarm: bool,
) -> Result<Prepared, TurnError> {
    if !capture {
        return Ok(Prepared {
            shot: None,
            capture: None,
            encode: None,
            prewarm: None,
        });
    }
    let taken = Instant::now();
    let capture = Arc::clone(&shared.capture);
    let max_edge = shared.config.max_edge;
    // DEEP gets its own, larger copy (one capture, two encodes).
    let deep_edge = shared.deep.is_some().then_some(shared.config.deep_max_edge);
    let (png, deep_png, capture_time, encode_time) =
        tokio::task::spawn_blocking(move || -> Result<_, CaptureError> {
            let started = Instant::now();
            let shot = capture.capture()?;
            let capture_time = started.elapsed();
            let started = Instant::now();
            let image = shot.encode(max_edge)?;
            let deep = deep_edge.map(|edge| shot.encode(edge)).transpose()?;
            Ok((
                image.png(),
                deep.map(|d| d.png()),
                capture_time,
                started.elapsed(),
            ))
        })
        .await
        .map_err(|_| TurnError::WorkerGone)??;
    let image = Part::Png(png);
    let deep_image = deep_png.map(Part::Png);

    let prewarm = if shared.config.prewarm && fast_prewarm {
        let started = Instant::now();
        let messages = shared.messages(Tier::Fast, history, Message::user(vec![image.clone()]));
        // A failed prewarm only costs latency; the question reports the error.
        let summary = run_to_end(&*shared.chat, shared.request(Tier::Fast, messages, 1)).await;
        Some(Prewarm {
            elapsed: started.elapsed(),
            timings: summary.ok().and_then(|s| s.timings),
        })
    } else {
        None
    };
    // Then DEEP, in the background, if it is loaded: FAST never waits for
    // it. A DEEP request then queues behind this on its server (--parallel
    // 1) and reuses the image.
    let loaded = shared.deep.as_ref().and_then(|d| d.loaded());
    if let (Some(deep), Some(deep_image), true) = (loaded, &deep_image, shared.config.deep_prewarm)
    {
        let messages = shared.messages(
            Tier::Deep,
            deep_history,
            Message::user(vec![deep_image.clone()]),
        );
        let mut request = shared.request(Tier::Deep, messages, 1);
        // No assistant turn opened: the prompt then ends right after the
        // image, which is where the question will continue (see session.rs).
        request
            .extra_body
            .insert("add_generation_prompt".into(), json!(false));
        tokio::spawn(async move {
            let _ = run_to_end(&*deep, request).await;
        });
    }
    Ok(Prepared {
        shot: Some(Shot {
            image,
            deep_image,
            taken,
        }),
        capture: Some(capture_time),
        encode: Some(encode_time),
        prewarm,
    })
}

async fn run_to_end(chat: &dyn ChatModel, request: ChatRequest) -> Result<ChatSummary, ChatError> {
    let mut stream = chat.chat(request).await?;
    while let Some(event) = stream.next().await {
        if let ChatEvent::Done(summary) = event? {
            return Ok(summary);
        }
    }
    Err(ChatError::Protocol("stream ended without a summary".into()))
}

/// Handed from the state machine to the task at key-up.
pub(crate) struct Started {
    pub(crate) turn: u64,
    pub(crate) chord: Chord,
    pub(crate) held: Duration,
    pub(crate) key_up: Instant,
    pub(crate) mic_started: Reply<Result<(), AudioError>>,
    pub(crate) recording: Reply<Result<Recording, AudioError>>,
    pub(crate) prepare: AbortOnDrop<Result<Prepared, TurnError>>,
    /// Earlier exchanges, already laid out for this turn, per tier.
    pub(crate) history: Vec<Message>,
    pub(crate) deep_history: Vec<Message>,
    /// The think chord was tapped, not held: re-ask without listening.
    pub(crate) tap: bool,
    /// The last question and DEEP's history before it, for a re-ask.
    pub(crate) reask: Option<Reask>,
}

/// What a "think harder" re-ask needs: the original question and the
/// history before it (screenshot included, FAST's answer left out).
pub(crate) struct Reask {
    pub(crate) question: String,
    pub(crate) history: Vec<Message>,
}

/// Where a turn's question goes.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    Fast(String),
    /// A new question for DEEP, on the same screenshot.
    Think(String),
    /// The last question again, to DEEP.
    Reask,
}

/// The maintainer's rule: strip the trigger phrase; nothing left means
/// re-ask the last question, otherwise the rest is a new question. The
/// think chord always means DEEP; a tap (no transcript) re-asks.
fn route(chord: Chord, transcript: Option<&str>, phrase: &str) -> Route {
    let Some(text) = transcript else {
        return Route::Reask;
    };
    let (found, rest) = strip_phrase(text, phrase);
    if chord == Chord::Think || found {
        if rest.is_empty() {
            Route::Reask
        } else {
            Route::Think(rest)
        }
    } else {
        Route::Fast(text.trim().to_owned())
    }
}

/// Remove every whole-word, case-insensitive occurrence of `phrase`. The
/// rest is trimmed of spaces and punctuation; with no letters or digits
/// left it is empty.
fn strip_phrase(text: &str, phrase: &str) -> (bool, String) {
    let phrase = phrase.trim().to_ascii_lowercase();
    let lower = text.to_ascii_lowercase();
    let mut rest = String::with_capacity(text.len());
    let mut found = false;
    let mut at = 0;
    while let Some(offset) = (!phrase.is_empty())
        .then(|| lower[at..].find(&phrase))
        .flatten()
    {
        let start = at + offset;
        let end = start + phrase.len();
        let boundary = |i: Option<char>| i.is_none_or(|c| !c.is_alphanumeric());
        if boundary(text[..start].chars().next_back()) && boundary(text[end..].chars().next()) {
            rest.push_str(&text[at..start]);
            rest.push(' ');
            found = true;
        } else {
            rest.push_str(&text[at..end]);
        }
        at = end;
    }
    rest.push_str(&text[at..]);
    let rest = rest.split_whitespace().collect::<Vec<_>>().join(" ");
    // Leading punctuation goes; at the end only separators, so a question
    // keeps its question mark.
    let rest = rest
        .trim_start_matches(|c: char| c.is_whitespace() || ",.!?;:-".contains(c))
        .trim_end_matches(|c: char| c.is_whitespace() || ",.;:-".contains(c));
    if rest.chars().any(char::is_alphanumeric) {
        (found, rest.to_owned())
    } else {
        (found, String::new())
    }
}

/// Key-up to the end of the turn. Always ends with exactly one
/// [`Internal::Done`] unless the task is aborted (then the state machine
/// writes the report).
pub(crate) async fn answer(
    shared: Arc<Shared>,
    started: Started,
    internal: UnboundedSender<Internal>,
) {
    let notify = Notifier {
        turn: started.turn,
        internal: &internal,
    };
    let key_up = started.key_up;
    let mut report = TurnReport::new(started.turn, started.chord, Outcome::Answered);
    report.held = Some(started.held);

    let commit = match run(&shared, started, &mut report, &notify).await {
        Ok(commit) => Some(commit),
        Err(error) => {
            shared.tts.stop();
            if let Some(line) = error.spoken_line() {
                notify.state(TurnState::Speaking);
                speak_line(&shared, line).await;
            }
            report.outcome = Outcome::Failed(error);
            None
        }
    };
    report.total = Some(key_up.elapsed());
    let _ = internal.send(Internal::Done(Box::new(report), commit));
}

async fn run(
    shared: &Shared,
    started: Started,
    report: &mut TurnReport,
    notify: &Notifier<'_>,
) -> Result<Commit, TurnError> {
    let Started {
        chord,
        key_up,
        tap,
        mic_started,
        recording,
        prepare,
        history,
        deep_history,
        reask,
        ..
    } = started;

    // What was asked. A think-chord tap has nothing to transcribe.
    let transcript = if tap {
        None
    } else {
        // Audio: the stop command was sent at key-up; it also resamples.
        let mic = |e: AudioError| TurnError::Mic(e.to_string());
        mic_started
            .await
            .map_err(|_| TurnError::WorkerGone)?
            .map_err(mic)?;
        let recording = recording
            .await
            .map_err(|_| TurnError::WorkerGone)?
            .map_err(mic)?;
        report.audio = Some(key_up.elapsed());
        report.mic_start = recording.start_latency;
        if recording.clip.is_silent() {
            return Err(TurnError::SilentMic);
        }
        let stt_started = Instant::now();
        let transcript = shared
            .stt
            .transcribe(recording.clip)
            .await
            .map_err(|_| TurnError::WorkerGone)?
            .map_err(|e| TurnError::Stt(e.to_string()))?;
        report.stt = Some(stt_started.elapsed());
        Some(transcript.text)
    };
    let route = route(chord, transcript.as_deref(), &shared.config.think_phrase);
    match &route {
        Route::Fast(text) if text.is_empty() => return Err(TurnError::NoSpeech),
        Route::Think(_) | Route::Reask if shared.deep.is_none() => return Err(TurnError::NoDeep),
        Route::Reask if reask.is_none() => return Err(TurnError::NothingToReask),
        _ => {}
    }

    // The question goes after the prewarm, so the server reuses the image.
    let wait_started = Instant::now();
    let prepared = prepare.await.map_err(|_| TurnError::WorkerGone)??;
    report.prewarm_wait = Some(wait_started.elapsed());
    report.capture = prepared.capture;
    report.encode = prepared.encode;
    report.fresh_image = Some(prepared.shot.is_some());
    report.history = Some(history.len() / 2);
    if let Some(prewarm) = &prepared.prewarm {
        report.prewarm = Some(prewarm.elapsed);
        if let Some(t) = &prewarm.timings {
            report.prewarm_prompt_n = t.prompt_n;
            report.prewarm_cache_n = t.cache_n;
        }
    }
    notify.state(TurnState::Thinking);
    let deep_image = prepared
        .shot
        .as_ref()
        .map(|shot| shot.for_tier(Tier::Deep).clone());

    let mut speech = shared.speech.lock().await;
    drain(&mut speech);
    let mut voice = Voice::new(shared, key_up, notify);
    let (question, answer, summary, replace_last) = match route {
        Route::Fast(text) => {
            // FAST: image and question in one message (measured in M0).
            let mut parts = Vec::with_capacity(2);
            if let Some(shot) = &prepared.shot {
                parts.push(shot.image.clone());
            }
            parts.push(Part::Text(text.clone()));
            let mut request = shared.request(
                Tier::Fast,
                shared.messages(Tier::Fast, history, Message::user(parts)),
                shared.config.max_tokens,
            );
            if shared.config.grammar && shared.escalates() {
                request.extra_body.insert("grammar".into(), json!(GRAMMAR));
            }
            let fast = shared.chat.chat(request).await?;
            let ended = voice
                .stream(&mut speech, fast, Some(EscalationGate::new()), report)
                .await?;
            report.ttft = voice.times.ttft;
            report.first_chunk = voice.times.first_chunk;
            match ended {
                Ended::Reply {
                    text: answer,
                    summary,
                } => (text, answer, summary, false),
                Ended::Signal(Decision::Escalate(reason)) if shared.escalates() => {
                    // One escalation per turn; DEEP's output is never gated.
                    report.escalation = Some(reason);
                    let (answer, summary) = deep_answer(
                        shared,
                        &mut voice,
                        &mut speech,
                        report,
                        notify,
                        filler(reason),
                        deep_history,
                        deep_image,
                        text.clone(),
                    )
                    .await?;
                    (text, answer, summary, false)
                }
                Ended::Signal(_) => return Err(TurnError::BadSignal),
            }
        }
        Route::Think(text) => {
            report.think = Some("new");
            let (answer, summary) = deep_answer(
                shared,
                &mut voice,
                &mut speech,
                report,
                notify,
                THINK_CUE,
                deep_history,
                deep_image,
                text.clone(),
            )
            .await?;
            (text, answer, summary, false)
        }
        Route::Reask => {
            report.think = Some("reask");
            let Reask { question, history } = reask.expect("checked above");
            // The original question and screenshot, not FAST's answer.
            let (answer, summary) = deep_answer(
                shared,
                &mut voice,
                &mut speech,
                report,
                notify,
                THINK_CUE,
                history,
                None,
                question.clone(),
            )
            .await?;
            (question, answer, summary, true)
        }
    };
    voice.finish(&mut speech, report).await?;

    if answer.trim().is_empty() {
        return Err(TurnError::EmptyReply);
    }
    report.sentences = u32::try_from(voice.queued.len()).unwrap_or(u32::MAX);
    if let Some(t) = summary.timings {
        report.prompt_n = t.prompt_n;
        report.cache_n = t.cache_n;
        report.tokens_per_second = t.predicted_per_second;
    }
    Ok(Commit {
        question,
        answer: answer.trim().to_owned(),
        shot: if replace_last { None } else { prepared.shot },
        replace_last,
    })
}

/// Ask DEEP, loading it first if needed, with spoken and visible progress:
/// a cue line, "Loading the bigger model first" when it is not loaded,
/// "Still thinking" during long silences. The prompt is prefilled first
/// (history and image, no assistant turn opened), so this and later
/// questions on the screenshot reuse it (see session.rs).
#[allow(clippy::too_many_arguments)]
async fn deep_answer(
    shared: &Shared,
    voice: &mut Voice<'_>,
    speech: &mut UnboundedReceiver<SpeechEvent>,
    report: &mut TurnReport,
    notify: &Notifier<'_>,
    cue: &str,
    history: Vec<Message>,
    image: Option<Part>,
    question: String,
) -> Result<(String, Box<ChatSummary>), TurnError> {
    let tier = shared.deep.as_ref().ok_or(TurnError::NoDeep)?;
    voice.say_cue(cue, Some(Mark::Filler))?;
    voice.nudge = Some(shared.config.deep_nudge);
    let model = match tier.loaded() {
        Some(model) => model,
        None => {
            voice.say_cue(LOADING_CUE, None)?;
            notify.progress(Progress::LoadingDeep);
            shared.deep_status(DeepStatus::Loading);
            let started = Instant::now();
            match voice.wait(speech, report, tier.load()).await? {
                Ok(model) => {
                    report.deep_load = Some(started.elapsed());
                    shared.deep_status(DeepStatus::Loaded);
                    model
                }
                Err(e) => {
                    shared.deep_status(DeepStatus::NotLoaded);
                    return Err(TurnError::DeepLoad(e));
                }
            }
        }
    };

    let mut prefix = history;
    if let Some(image) = image {
        prefix.push(Message::user(vec![image]));
    }
    notify.progress(Progress::ReadingScreen);
    let started = Instant::now();
    let mut prefill = shared.request(Tier::Deep, shared.prefix(Tier::Deep, prefix.clone()), 1);
    prefill
        .extra_body
        .insert("add_generation_prompt".into(), json!(false));
    voice
        .wait(speech, report, run_to_end(&*model, prefill))
        .await??;
    report.deep_prefill = Some(started.elapsed());

    notify.progress(Progress::ThinkingHarder);
    let request = shared.request(
        Tier::Deep,
        shared.messages(Tier::Deep, prefix, Message::user_text(question)),
        shared.config.deep_max_tokens,
    );
    voice.times = Times::default();
    voice.next_mark = Some(Mark::Deep);
    let stream = model.chat(request).await?;
    let ended = voice.stream(speech, stream, None, report).await?;
    voice.nudge = None;
    report.deep_ttft = voice.times.ttft;
    report.deep_thinking = voice.times.thinking();
    match ended {
        Ended::Reply { text, summary } => Ok((text, summary)),
        Ended::Signal(_) => unreachable!("DEEP is never gated"),
    }
}

/// The fixed line spoken while DEEP gets going, by escalation reason. Local
/// text only: nothing from the screen or FAST is ever in it.
pub(crate) fn filler(reason: Reason) -> &'static str {
    match reason {
        Reason::Reasoning => "Let me think about that.",
        Reason::Detail => "Let me take a closer look.",
        Reason::CodeMath => "Let me work that out.",
        Reason::Uncertain => "Let me check that more carefully.",
        Reason::UserAsked => "Sure, let me look into it.",
    }
}

/// How a reply stream ended.
enum Ended {
    Reply {
        text: String,
        summary: Box<ChatSummary>,
    },
    /// The gate stopped the stream: escalate or invalid.
    Signal(Decision),
}

#[derive(Default)]
struct Times {
    /// First answer token.
    ttft: Option<Duration>,
    first_chunk: Option<Duration>,
    first_reasoning: Option<Duration>,
}

impl Times {
    /// First thinking token to first answer token.
    fn thinking(&self) -> Option<Duration> {
        Some(self.ttft?.saturating_sub(self.first_reasoning?))
    }
}

/// Utterances whose start is worth timing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Filler,
    Deep,
}

/// The turn's speech queue: FAST's sentences, or the filler then DEEP's.
struct Voice<'a> {
    shared: &'a Shared,
    notify: &'a Notifier<'a>,
    key_up: Instant,
    queued: Vec<UtteranceId>,
    ended: usize,
    marks: Vec<(UtteranceId, Mark)>,
    /// The next sentence queued gets this mark.
    next_mark: Option<Mark>,
    times: Times,
    /// Say "Still thinking" after this long with nothing being said.
    nudge: Option<Duration>,
    /// When the last utterance ended (or the turn's speech began).
    quiet_since: Instant,
}

impl<'a> Voice<'a> {
    fn new(shared: &'a Shared, key_up: Instant, notify: &'a Notifier<'a>) -> Self {
        Self {
            shared,
            notify,
            key_up,
            queued: Vec::new(),
            ended: 0,
            marks: Vec::new(),
            next_mark: None,
            times: Times::default(),
            nudge: None,
            quiet_since: Instant::now(),
        }
    }

    /// A fixed cue line: queued like a sentence, but not counted as the
    /// reply's first chunk.
    fn say_cue(&mut self, text: &str, mark: Option<Mark>) -> Result<(), TurnError> {
        let first_chunk = self.times.first_chunk;
        self.next_mark = mark;
        self.say(text)?;
        self.times.first_chunk = first_chunk;
        Ok(())
    }

    /// When to nudge next: only while nothing is queued or playing.
    fn nudge_deadline(&self) -> Option<Instant> {
        let nudge = self.nudge?;
        (self.ended >= self.queued.len()).then_some(self.quiet_since + nudge)
    }

    /// Await `work` while following speech events and nudging.
    async fn wait<F: Future>(
        &mut self,
        speech: &mut UnboundedReceiver<SpeechEvent>,
        report: &mut TurnReport,
        work: F,
    ) -> Result<F::Output, TurnError> {
        tokio::pin!(work);
        loop {
            let nudge = self.nudge_deadline();
            tokio::select! {
                out = &mut work => return Ok(out),
                event = speech.recv() => self.on_event(event, report)?,
                () = sleep_until_opt(nudge) => self.say_cue(NUDGE_CUE, None)?,
            }
        }
    }

    fn say(&mut self, text: &str) -> Result<(), TurnError> {
        if text.trim().is_empty() {
            return Ok(());
        }
        self.times
            .first_chunk
            .get_or_insert_with(|| self.key_up.elapsed());
        let id = self
            .shared
            .tts
            .speak(text)
            .map_err(|e| TurnError::Speech(e.to_string()))?;
        self.queued.push(id);
        if let Some(mark) = self.next_mark.take() {
            self.marks.push((id, mark));
        }
        Ok(())
    }

    fn on_event(
        &mut self,
        event: Option<SpeechEvent>,
        report: &mut TurnReport,
    ) -> Result<(), TurnError> {
        match event {
            None => return Err(TurnError::Speech("the speech engine stopped".into())),
            Some(SpeechEvent::Started(id)) if self.queued.contains(&id) => {
                let at = self.key_up.elapsed();
                if report.ttfa.is_none() {
                    report.ttfa = Some(at);
                    self.notify.state(TurnState::Speaking);
                }
                match self
                    .marks
                    .iter()
                    .find(|(m, _)| *m == id)
                    .map(|(_, mark)| *mark)
                {
                    Some(Mark::Filler) => report.filler_audio = Some(at),
                    Some(Mark::Deep) => report.deep_first_audio = Some(at),
                    None => {}
                }
            }
            Some(SpeechEvent::Finished(id) | SpeechEvent::Cancelled(id))
                if self.queued.contains(&id) =>
            {
                self.ended += 1;
                if self.ended >= self.queued.len() {
                    self.quiet_since = Instant::now();
                }
            }
            Some(_) => {}
        }
        Ok(())
    }

    /// Read `stream` to its end, speaking sentence by sentence, while
    /// following speech events. With a gate, stop as soon as it decides the
    /// reply is a signal (the stream is dropped, which cancels it).
    async fn stream(
        &mut self,
        speech: &mut UnboundedReceiver<SpeechEvent>,
        mut stream: ChatStream,
        mut gate: Option<EscalationGate>,
        report: &mut TurnReport,
    ) -> Result<Ended, TurnError> {
        let mut chunker = SentenceChunker::new(ChunkerConfig::default());
        let mut text = String::new();
        loop {
            let nudge = self.nudge_deadline();
            tokio::select! {
                event = stream.next() => match event {
                    Some(Ok(ChatEvent::Content(delta))) => {
                        self.times.ttft.get_or_insert_with(|| self.key_up.elapsed());
                        // The answer has begun: no more "still thinking".
                        self.nudge = None;
                        let speakable = match gate.as_mut().map(|g| g.push(&delta)) {
                            None => delta,
                            Some(Step::Speak(s)) => s,
                            Some(Step::Wait) => String::new(),
                            Some(Step::Stop(decision)) => return Ok(Ended::Signal(decision)),
                        };
                        text.push_str(&speakable);
                        for chunk in chunker.push(&speakable) {
                            self.say(&chunk)?;
                        }
                    }
                    // Thinking text is never spoken.
                    Some(Ok(ChatEvent::Reasoning(_))) => {
                        self.times
                            .first_reasoning
                            .get_or_insert_with(|| self.key_up.elapsed());
                    }
                    Some(Ok(ChatEvent::Done(summary))) => {
                        if let Some(gate) = gate.as_mut() {
                            let decision = gate.finish();
                            if decision != Decision::Answer {
                                return Ok(Ended::Signal(decision));
                            }
                        }
                        if let Some(rest) = chunker.finish() {
                            self.say(&rest)?;
                        }
                        return Ok(Ended::Reply {
                            text,
                            summary: Box::new(summary),
                        });
                    }
                    Some(Err(e)) => return Err(e.into()),
                    None => return Err(TurnError::Chat("the stream ended early".into())),
                },
                event = speech.recv() => self.on_event(event, report)?,
                () = sleep_until_opt(nudge) => self.say_cue(NUDGE_CUE, None)?,
            }
        }
    }

    /// Wait until everything queued has been spoken or cancelled.
    async fn finish(
        &mut self,
        speech: &mut UnboundedReceiver<SpeechEvent>,
        report: &mut TurnReport,
    ) -> Result<(), TurnError> {
        while self.ended < self.queued.len() {
            let event = speech.recv().await;
            self.on_event(event, report)?;
        }
        Ok(())
    }
}

/// Speak one fixed line and wait until it is over (or a timeout passes).
async fn speak_line(shared: &Shared, line: &str) {
    let mut speech = shared.speech.lock().await;
    drain(&mut speech);
    let Ok(id) = shared.tts.speak(line) else {
        return;
    };
    let over = async {
        while let Some(event) = speech.recv().await {
            if let SpeechEvent::Finished(e) | SpeechEvent::Cancelled(e) = event
                && e == id
            {
                return;
            }
        }
    };
    let _ = tokio::time::timeout(ERROR_LINE_TIMEOUT, over).await;
}

async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Throw away events left over from earlier turns.
fn drain(speech: &mut UnboundedReceiver<SpeechEvent>) {
    while speech.try_recv().is_ok() {}
}

#[cfg(test)]
mod route_tests {
    use super::*;

    const P: &str = "think harder";

    #[test]
    fn the_phrase_alone_re_asks() {
        for text in [
            "Think harder.",
            "think harder",
            "  THINK HARDER!  ",
            "Think harder, please?",
        ] {
            let expect = if text.contains("please") {
                Route::Think("please?".into())
            } else {
                Route::Reask
            };
            assert_eq!(route(Chord::Talk, Some(text), P), expect, "{text}");
        }
    }

    #[test]
    fn the_rest_becomes_a_new_question() {
        assert_eq!(
            route(Chord::Talk, Some("Think harder: what is the total?"), P),
            Route::Think("what is the total?".into())
        );
        assert_eq!(
            route(Chord::Look, Some("What is the total? Think harder."), P),
            Route::Think("What is the total?".into())
        );
    }

    #[test]
    fn other_words_containing_it_do_not_trigger() {
        assert_eq!(
            route(Chord::Talk, Some("Do I rethink harderware?"), P),
            Route::Fast("Do I rethink harderware?".into())
        );
        assert_eq!(
            route(Chord::Talk, Some("What is this?"), P),
            Route::Fast("What is this?".into())
        );
    }

    #[test]
    fn the_think_chord_always_goes_to_deep() {
        assert_eq!(route(Chord::Think, None, P), Route::Reask);
        assert_eq!(route(Chord::Think, Some(""), P), Route::Reask);
        assert_eq!(
            route(Chord::Think, Some("Why is this slow?"), P),
            Route::Think("Why is this slow?".into())
        );
    }
}
