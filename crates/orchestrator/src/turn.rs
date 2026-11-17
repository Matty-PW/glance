//! What one turn does once it is running: the key-down preparation (capture,
//! encode, prewarm) and the key-up answer (transcribe, ask FAST, speak; or
//! escalate: filler line, ask DEEP, speak).

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
use crate::{Chord, Outcome, Parts, TurnConfig, TurnError, TurnReport, TurnState};

/// How long a fixed error line may take to be spoken before giving up.
const ERROR_LINE_TIMEOUT: Duration = Duration::from_secs(15);

/// Everything a turn needs, shared between the state machine and the tasks
/// it spawns.
pub(crate) struct Shared {
    pub(crate) capture: Arc<dyn ScreenCapture>,
    pub(crate) mic: MicWorker,
    pub(crate) stt: SttWorker,
    pub(crate) chat: Arc<dyn ChatModel>,
    /// The DEEP tier, if one is configured.
    pub(crate) deep: Option<Arc<dyn ChatModel>>,
    pub(crate) tts: Arc<dyn TextToSpeech>,
    /// Held by whichever turn is speaking. Events carry utterance ids, so a
    /// turn ignores anything left over from a cancelled one.
    pub(crate) speech: Mutex<UnboundedReceiver<SpeechEvent>>,
    pub(crate) config: TurnConfig,
}

impl Shared {
    pub(crate) fn new(parts: Parts, config: TurnConfig) -> Self {
        Self {
            capture: parts.capture,
            mic: MicWorker::spawn(parts.mic),
            stt: SttWorker::spawn(parts.stt),
            chat: parts.chat,
            deep: parts.deep,
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

    /// FAST's prompt offers escalation only when there is a DEEP tier
    /// (`docs/DESIGN.md` §7.3, rule 4). Prewarm and question must use the
    /// same one, or the cached prefix would not match.
    fn system(&self, tier: Tier) -> Message {
        let prompt = match (tier, self.deep.is_some()) {
            (Tier::Deep, _) => &self.config.deep_system_prompt,
            (Tier::Fast, true) => &self.config.escalation_prompt,
            (Tier::Fast, false) => &self.config.system_prompt,
        };
        Message::system(prompt.clone())
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

    let prewarm = if shared.config.prewarm {
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
    // Then DEEP, in the background: FAST never waits for it. If the turn
    // escalates, DEEP's question queues behind this on its server
    // (--parallel 1) and reuses the image.
    if let (Some(deep), Some(deep_image), true) =
        (&shared.deep, &deep_image, shared.config.deep_prewarm)
    {
        let deep = Arc::clone(deep);
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
        key_up,
        mic_started,
        recording,
        prepare,
        history,
        deep_history,
        ..
    } = started;

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
    if transcript.text.is_empty() {
        return Err(TurnError::NoSpeech);
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

    // FAST: image and question in one message (measured in M0).
    let fast_question = {
        let mut parts = Vec::with_capacity(2);
        if let Some(shot) = &prepared.shot {
            parts.push(shot.image.clone());
        }
        parts.push(Part::Text(transcript.text.clone()));
        Message::user(parts)
    };
    let mut request = shared.request(
        Tier::Fast,
        shared.messages(Tier::Fast, history, fast_question),
        shared.config.max_tokens,
    );
    if shared.config.grammar {
        request.extra_body.insert("grammar".into(), json!(GRAMMAR));
    }
    let fast = shared.chat.chat(request).await?;

    let mut speech = shared.speech.lock().await;
    drain(&mut speech);
    let mut voice = Voice::new(shared, key_up, notify);
    let ended = voice
        .stream(&mut speech, fast, Some(EscalationGate::new()), report)
        .await?;
    report.ttft = voice.times.ttft;
    report.first_chunk = voice.times.first_chunk;

    let (answer, summary) = match ended {
        Ended::Reply { text, summary } => (text, summary),
        Ended::Signal(Decision::Escalate(reason)) if shared.deep.is_some() => {
            // One escalation per turn; DEEP's output is never gated.
            report.escalation = Some(reason);
            voice.say_marked(filler(reason), Mark::Filler)?;
            let deep = shared.deep.as_ref().expect("checked above");
            // DEEP: the image as its own message, then the question, so the
            // prompt continues exactly where the prewarm stopped.
            let mut deep_history = deep_history;
            if let Some(shot) = &prepared.shot {
                deep_history.push(Message::user(vec![shot.for_tier(Tier::Deep).clone()]));
            }
            let request = shared.request(
                Tier::Deep,
                shared.messages(
                    Tier::Deep,
                    deep_history,
                    Message::user_text(transcript.text.clone()),
                ),
                shared.config.deep_max_tokens,
            );
            voice.times = Times::default();
            voice.next_mark = Some(Mark::Deep);
            let stream = deep.chat(request).await?;
            let ended = voice.stream(&mut speech, stream, None, report).await?;
            report.deep_ttft = voice.times.ttft;
            match ended {
                Ended::Reply { text, summary } => (text, summary),
                Ended::Signal(_) => unreachable!("DEEP is never gated"),
            }
        }
        Ended::Signal(_) => return Err(TurnError::BadSignal),
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
        question: transcript.text,
        answer: answer.trim().to_owned(),
        shot: prepared.shot,
    })
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
    ttft: Option<Duration>,
    first_chunk: Option<Duration>,
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

    fn say_marked(&mut self, text: &str, mark: Mark) -> Result<(), TurnError> {
        self.next_mark = Some(mark);
        self.say(text)
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
            tokio::select! {
                event = stream.next() => match event {
                    Some(Ok(ChatEvent::Content(delta))) => {
                        self.times.ttft.get_or_insert_with(|| self.key_up.elapsed());
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
                    Some(Ok(ChatEvent::Reasoning(_))) => {}
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

/// Throw away events left over from earlier turns.
fn drain(speech: &mut UnboundedReceiver<SpeechEvent>) {
    while speech.try_recv().is_ok() {}
}
