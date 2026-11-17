//! The per-turn latency line and the turn error type.
//!
//! Both are safe to log: they hold timings, counts and fixed error text,
//! never transcript or reply text and never image or audio data.

use std::fmt;
use std::time::Duration;

use glance_capture::CaptureError;
use glance_llm_client::ChatError;
use glance_llm_client::escalate::Reason;

use crate::Chord;

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The reply was spoken to the end.
    Answered,
    /// The press was shorter than `min_press`, so it only cancelled.
    ShortPress,
    /// Cancelled by a new press, or by shutdown.
    Cancelled,
    Failed(TurnError),
}

/// Why a turn failed. Messages are fixed text plus device or HTTP status
/// details, so they can be shown and logged.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TurnError {
    #[error("Screen Recording permission is not granted")]
    ScreenPermission,
    #[error("screen capture failed: {0}")]
    Capture(String),
    #[error("microphone failed: {0}")]
    Mic(String),
    #[error("the microphone delivered pure silence (is Microphone permission granted?)")]
    SilentMic,
    #[error("speech recognition failed: {0}")]
    Stt(String),
    #[error("no speech was recognized")]
    NoSpeech,
    #[error("model request failed: {0}")]
    Chat(String),
    #[error("the model sent an empty reply")]
    EmptyReply,
    #[error("the fast model's reply started like an escalation but was not a valid one")]
    BadSignal,
    #[error("speech output failed: {0}")]
    Speech(String),
    #[error("an internal worker stopped")]
    WorkerGone,
}

impl TurnError {
    /// Short stable code for the log line.
    pub fn code(&self) -> &'static str {
        match self {
            TurnError::ScreenPermission => "screen_permission",
            TurnError::Capture(_) => "capture",
            TurnError::Mic(_) => "mic",
            TurnError::SilentMic => "silent_mic",
            TurnError::Stt(_) => "stt",
            TurnError::NoSpeech => "no_speech",
            TurnError::Chat(_) => "chat",
            TurnError::EmptyReply => "empty_reply",
            TurnError::BadSignal => "bad_signal",
            TurnError::Speech(_) => "speech",
            TurnError::WorkerGone => "worker_gone",
        }
    }

    /// A fixed local line spoken when the turn fails, so a failure is not
    /// silent in an app with no window. `None` when speech itself is broken.
    pub fn spoken_line(&self) -> Option<&'static str> {
        Some(match self {
            TurnError::ScreenPermission => {
                "I can't see the screen. Check the Screen Recording permission for glance."
            }
            TurnError::Capture(_) => "Sorry, I couldn't capture the screen.",
            TurnError::Mic(_) | TurnError::SilentMic => {
                "I couldn't hear anything. Check the Microphone permission for glance."
            }
            TurnError::Stt(_) => "Sorry, speech recognition failed.",
            TurnError::NoSpeech => "Sorry, I didn't catch that.",
            TurnError::Chat(_) => "Sorry, I couldn't reach the model.",
            TurnError::EmptyReply => "Sorry, I don't have an answer for that.",
            TurnError::BadSignal => "Sorry, ask me again.",
            TurnError::Speech(_) | TurnError::WorkerGone => return None,
        })
    }
}

impl From<CaptureError> for TurnError {
    fn from(e: CaptureError) -> Self {
        match e {
            CaptureError::PermissionDenied => TurnError::ScreenPermission,
            other => TurnError::Capture(other.to_string()),
        }
    }
}

impl From<ChatError> for TurnError {
    fn from(e: ChatError) -> Self {
        // Error bodies and protocol fragments are not logged: they could echo
        // text from the prompt.
        TurnError::Chat(match e {
            ChatError::Connect(e) => format!("could not reach the endpoint ({e})"),
            ChatError::Status { status, .. } => format!("HTTP {status}"),
            ChatError::Stream(_) => "the stream broke".into(),
            ChatError::Protocol(_) => "the endpoint sent an invalid stream".into(),
        })
    }
}

/// One line per turn with every stage's timing (`docs/DESIGN.md` §12).
/// Times after the key-up (`ttft`, `first_chunk`, `ttfa`, `total`) are
/// measured from the key-up.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnReport {
    pub turn: u64,
    pub chord: Chord,
    pub outcome: Outcome,
    /// Whether the question came with a new screenshot (`false` for a
    /// follow-up on the session's screenshot).
    pub fresh_image: Option<bool>,
    /// Earlier exchanges sent with the question.
    pub history: Option<usize>,
    /// How long the chord was held (the recording length).
    pub held: Option<Duration>,
    /// Delay before the microphone delivered its first samples.
    pub mic_start: Option<Duration>,
    pub capture: Option<Duration>,
    pub encode: Option<Duration>,
    pub prewarm: Option<Duration>,
    pub prewarm_prompt_n: Option<u64>,
    pub prewarm_cache_n: Option<u64>,
    /// Stopping the microphone and resampling to 16 kHz.
    pub audio: Option<Duration>,
    pub stt: Option<Duration>,
    /// Time spent after STT waiting for the prewarm to finish.
    pub prewarm_wait: Option<Duration>,
    pub ttft: Option<Duration>,
    pub first_chunk: Option<Duration>,
    /// Key-up to the first sound: the metric that matters.
    pub ttfa: Option<Duration>,
    pub total: Option<Duration>,
    /// Set when FAST handed the question to DEEP.
    pub escalation: Option<Reason>,
    /// Key-up to the filler line's first sound.
    pub filler_audio: Option<Duration>,
    /// Key-up to DEEP's first token and first sound.
    pub deep_ttft: Option<Duration>,
    pub deep_first_audio: Option<Duration>,
    pub prompt_n: Option<u64>,
    pub cache_n: Option<u64>,
    pub tokens_per_second: Option<f64>,
    pub sentences: u32,
}

impl TurnReport {
    pub fn new(turn: u64, chord: Chord, outcome: Outcome) -> Self {
        Self {
            turn,
            chord,
            outcome,
            fresh_image: None,
            history: None,
            held: None,
            mic_start: None,
            capture: None,
            encode: None,
            prewarm: None,
            prewarm_prompt_n: None,
            prewarm_cache_n: None,
            audio: None,
            stt: None,
            prewarm_wait: None,
            ttft: None,
            first_chunk: None,
            ttfa: None,
            total: None,
            escalation: None,
            filler_audio: None,
            deep_ttft: None,
            deep_first_audio: None,
            prompt_n: None,
            cache_n: None,
            tokens_per_second: None,
            sentences: 0,
        }
    }
}

impl fmt::Display for TurnReport {
    /// `turn=3 chord=talk outcome=answered ttfa_ms=512.3 …`; absent fields
    /// are left out.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "turn={} chord={}", self.turn, self.chord.as_str())?;
        match &self.outcome {
            Outcome::Answered => write!(f, " outcome=answered")?,
            Outcome::ShortPress => write!(f, " outcome=short_press")?,
            Outcome::Cancelled => write!(f, " outcome=cancelled")?,
            Outcome::Failed(e) => write!(f, " outcome=failed error={}", e.code())?,
        }
        if let Some(fresh) = self.fresh_image {
            write!(f, " image={}", if fresh { "fresh" } else { "reused" })?;
        }
        if let Some(n) = self.history {
            write!(f, " history={n}")?;
        }
        if let Some(reason) = self.escalation {
            write!(f, " escalated={}", reason.as_str())?;
        }
        let times = [
            ("ttfa_ms", self.ttfa),
            ("held_ms", self.held),
            ("mic_start_ms", self.mic_start),
            ("capture_ms", self.capture),
            ("encode_ms", self.encode),
            ("prewarm_ms", self.prewarm),
            ("audio_ms", self.audio),
            ("stt_ms", self.stt),
            ("prewarm_wait_ms", self.prewarm_wait),
            ("ttft_ms", self.ttft),
            ("first_chunk_ms", self.first_chunk),
            ("filler_audio_ms", self.filler_audio),
            ("deep_ttft_ms", self.deep_ttft),
            ("deep_audio_ms", self.deep_first_audio),
            ("total_ms", self.total),
        ];
        for (name, value) in times {
            if let Some(d) = value {
                write!(f, " {name}={:.1}", d.as_secs_f64() * 1000.0)?;
            }
        }
        let counts = [
            ("prewarm_prompt_n", self.prewarm_prompt_n),
            ("prewarm_cache_n", self.prewarm_cache_n),
            ("prompt_n", self.prompt_n),
            ("cache_n", self.cache_n),
        ];
        for (name, value) in counts {
            if let Some(n) = value {
                write!(f, " {name}={n}")?;
            }
        }
        if let Some(rate) = self.tokens_per_second {
            write!(f, " tok_s={rate:.1}")?;
        }
        if matches!(self.outcome, Outcome::Answered) {
            write!(f, " sentences={}", self.sentences)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_lists_present_fields_only() {
        let mut report = TurnReport::new(3, Chord::Talk, Outcome::Answered);
        report.ttfa = Some(Duration::from_micros(512_340));
        report.cache_n = Some(660);
        report.sentences = 2;
        assert_eq!(
            report.to_string(),
            "turn=3 chord=talk outcome=answered ttfa_ms=512.3 cache_n=660 sentences=2"
        );
    }

    #[test]
    fn failure_shows_code_only() {
        let report = TurnReport::new(
            1,
            Chord::Look,
            Outcome::Failed(TurnError::Chat("HTTP 500".into())),
        );
        assert_eq!(
            report.to_string(),
            "turn=1 chord=look outcome=failed error=chat"
        );
    }

    #[test]
    fn chat_error_bodies_are_not_kept() {
        let e = TurnError::from(ChatError::Status {
            status: 400,
            body: "echo of the user's words".into(),
        });
        assert_eq!(e, TurnError::Chat("HTTP 400".into()));
    }
}
