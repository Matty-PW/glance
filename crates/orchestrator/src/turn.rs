//! What one turn does once it is running: the key-down preparation (capture,
//! encode, prewarm) and the key-up answer (transcribe, ask, speak).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use glance_audio::{AudioError, Recording};
use glance_capture::{CaptureError, ScreenCapture};
use glance_llm_client::{
    ChatError, ChatEvent, ChatModel, ChatRequest, ChatSummary, Message, Part, Timings,
};
use glance_tts::{ChunkerConfig, SentenceChunker, SpeechEvent, TextToSpeech, UtteranceId};
use tokio::sync::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::task::{JoinError, JoinHandle};
use tokio::time::Instant;

use crate::session::{Commit, Shot};
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
            tts: parts.tts,
            speech: Mutex::new(parts.speech_events),
            config,
        }
    }

    fn request(&self, messages: Vec<Message>, max_tokens: u32) -> ChatRequest {
        ChatRequest {
            messages,
            max_tokens: Some(max_tokens),
            temperature: Some(0.0),
            extra_body: self.config.extra_body.clone(),
        }
    }

    fn system(&self) -> Message {
        Message::system(self.config.system_prompt.clone())
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
    let (png, capture_time, encode_time) =
        tokio::task::spawn_blocking(move || -> Result<_, CaptureError> {
            let started = Instant::now();
            let shot = capture.capture()?;
            let capture_time = started.elapsed();
            let started = Instant::now();
            let image = shot.encode(max_edge)?;
            Ok((image.png(), capture_time, started.elapsed()))
        })
        .await
        .map_err(|_| TurnError::WorkerGone)??;
    let image = Part::Png(png);

    let prewarm = if shared.config.prewarm {
        let started = Instant::now();
        let mut messages = Vec::with_capacity(history.len() + 2);
        messages.push(shared.system());
        messages.extend(history);
        messages.push(Message::user(vec![image.clone()]));
        // A failed prewarm only costs latency; the question reports the error.
        let summary = run_to_end(&*shared.chat, shared.request(messages, 1)).await;
        Some(Prewarm {
            elapsed: started.elapsed(),
            timings: summary.ok().and_then(|s| s.timings),
        })
    } else {
        None
    };
    Ok(Prepared {
        shot: Some(Shot { image, taken }),
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
    /// Earlier exchanges, already laid out for this turn.
    pub(crate) history: Vec<Message>,
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

    let mut question = Vec::with_capacity(2);
    if let Some(shot) = &prepared.shot {
        question.push(shot.image.clone());
    }
    question.push(Part::Text(transcript.text.clone()));
    let mut messages = Vec::with_capacity(history.len() + 2);
    messages.push(shared.system());
    messages.extend(history);
    messages.push(Message::user(question));
    let mut stream = shared
        .chat
        .chat(shared.request(messages, shared.config.max_tokens))
        .await?;

    let mut speech = shared.speech.lock().await;
    drain(&mut speech);
    let mut chunker = SentenceChunker::new(ChunkerConfig::default());
    let mut queued: Vec<UtteranceId> = Vec::new();
    let mut ended = 0;
    let mut summary: Option<ChatSummary> = None;
    // The reply as the model wrote it, kept for the next turn's history.
    let mut answer = String::new();
    let queue = |chunk: String, queued: &mut Vec<UtteranceId>, report: &mut TurnReport| {
        if chunk.trim().is_empty() {
            return Ok(());
        }
        report.first_chunk.get_or_insert_with(|| key_up.elapsed());
        let id = shared
            .tts
            .speak(&chunk)
            .map_err(|e| TurnError::Speech(e.to_string()))?;
        queued.push(id);
        Ok::<(), TurnError>(())
    };

    while summary.is_none() || ended < queued.len() {
        tokio::select! {
            event = stream.next(), if summary.is_none() => match event {
                Some(Ok(ChatEvent::Content(text))) => {
                    report.ttft.get_or_insert_with(|| key_up.elapsed());
                    answer.push_str(&text);
                    for chunk in chunker.push(&text) {
                        queue(chunk, &mut queued, report)?;
                    }
                }
                // Thinking text is never spoken.
                Some(Ok(ChatEvent::Reasoning(_))) => {}
                Some(Ok(ChatEvent::Done(done))) => {
                    if let Some(rest) = chunker.finish() {
                        queue(rest, &mut queued, report)?;
                    }
                    summary = Some(done);
                }
                Some(Err(e)) => return Err(e.into()),
                None => return Err(TurnError::Chat("the stream ended early".into())),
            },
            event = speech.recv() => match event {
                None => return Err(TurnError::Speech("the speech engine stopped".into())),
                Some(SpeechEvent::Started(id)) if queued.contains(&id) => {
                    if report.ttfa.is_none() {
                        report.ttfa = Some(key_up.elapsed());
                        notify.state(TurnState::Speaking);
                    }
                }
                Some(SpeechEvent::Finished(id) | SpeechEvent::Cancelled(id))
                    if queued.contains(&id) => ended += 1,
                Some(_) => {}
            },
        }
    }
    drop(stream);

    if queued.is_empty() {
        return Err(TurnError::EmptyReply);
    }
    report.sentences = u32::try_from(queued.len()).unwrap_or(u32::MAX);
    if let Some(t) = summary.and_then(|s| s.timings) {
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
