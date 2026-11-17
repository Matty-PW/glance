//! The escalation signal (`docs/DESIGN.md` §7.3).
//!
//! FAST either answers in plain speech or outputs exactly
//! `{"glance":"escalate","v":1,"reason":"<reason>"}` and nothing else. The
//! [`EscalationGate`] watches the start of FAST's stream:
//!
//! 1. First non-whitespace character is not `{`: an answer. Text flows
//!    straight through, so the normal path pays no extra latency.
//! 2. It is `{`: buffer up to the first `}` (hard cap 160 bytes) and parse
//!    strictly: no unknown fields, exact constants, closed reason enum.
//! 3. Anything else that starts with `{` is invalid. It never escalates and
//!    is never spoken.
//!
//! Screen text cannot steer this beyond choosing one of five reasons, which
//! are used only for metrics and to pick a filler line. DEEP's output is
//! never passed through a gate.

use serde::Deserialize;

/// Bytes of a `{…}` reply buffered before giving up on it.
pub const MAX_SIGNAL_BYTES: usize = 160;

/// Why FAST asked for DEEP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Reasoning,
    Detail,
    CodeMath,
    Uncertain,
    UserAsked,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Reasoning => "reasoning",
            Reason::Detail => "detail",
            Reason::CodeMath => "code_math",
            Reason::Uncertain => "uncertain",
            Reason::UserAsked => "user_asked",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
// `glance` and `v` are checked while deserializing and not needed after.
#[allow(dead_code)]
struct Signal {
    glance: Constant<Escalate>,
    v: Constant<One>,
    reason: Reason,
}

/// A field that must hold one exact value.
struct Constant<T>(std::marker::PhantomData<T>);

trait Exact {
    fn matches(value: &serde_json::Value) -> bool;
}
struct Escalate;
impl Exact for Escalate {
    fn matches(value: &serde_json::Value) -> bool {
        value.as_str() == Some("escalate")
    }
}
struct One;
impl Exact for One {
    fn matches(value: &serde_json::Value) -> bool {
        value.as_u64() == Some(1) && value.is_u64()
    }
}

impl<'de, T: Exact> Deserialize<'de> for Constant<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        if T::matches(&value) {
            Ok(Constant(std::marker::PhantomData))
        } else {
            Err(serde::de::Error::custom("unexpected constant"))
        }
    }
}

/// What the gate has decided about the reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// A spoken answer: everything flows through.
    Answer,
    /// A valid escalate object. Cancel FAST's stream now.
    Escalate(Reason),
    /// Started with `{` but is not a valid signal. Speak nothing of it.
    Invalid,
}

/// The result of feeding one delta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Not decided yet; nothing to speak.
    Wait,
    /// The reply is an answer; speak this text (may be empty).
    Speak(String),
    /// The reply was a signal: escalate or invalid. Stop reading the stream.
    Stop(Decision),
}

#[derive(Debug, Default)]
pub struct EscalationGate {
    buffer: String,
    decided: Option<Decision>,
}

impl EscalationGate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn decision(&self) -> Option<Decision> {
        self.decided
    }

    /// Feed the next piece of FAST's reply.
    pub fn push(&mut self, delta: &str) -> Step {
        match self.decided {
            Some(Decision::Answer) => return Step::Speak(delta.to_owned()),
            Some(other) => return Step::Stop(other),
            None => {}
        }
        self.buffer.push_str(delta);
        let Some(first) = self.buffer.trim_start().chars().next() else {
            return Step::Wait;
        };
        if first != '{' {
            self.decided = Some(Decision::Answer);
            return Step::Speak(std::mem::take(&mut self.buffer));
        }
        if let Some(close) = self.buffer.find('}') {
            let decision = parse(&self.buffer[..=close]);
            self.decided = Some(decision);
            return Step::Stop(decision);
        }
        if self.buffer.len() > MAX_SIGNAL_BYTES {
            self.decided = Some(Decision::Invalid);
            return Step::Stop(Decision::Invalid);
        }
        Step::Wait
    }

    /// The stream ended. An empty reply counts as an (empty) answer; an
    /// unfinished `{…` is invalid.
    pub fn finish(&mut self) -> Decision {
        let decision = match self.decided {
            Some(decision) => decision,
            None if self.buffer.trim().is_empty() => Decision::Answer,
            None => Decision::Invalid,
        };
        self.decided = Some(decision);
        decision
    }
}

fn parse(object: &str) -> Decision {
    match serde_json::from_str::<Signal>(object.trim()) {
        Ok(signal) => {
            let Signal {
                glance: _,
                v: _,
                reason,
            } = signal;
            Decision::Escalate(reason)
        }
        Err(_) => Decision::Invalid,
    }
}

/// A GBNF grammar for llama-server that allows only the two legal shapes:
/// an answer that does not start with `{` or whitespace, or the exact
/// escalate object. With it, an invalid signal cannot be generated.
pub const GRAMMAR: &str = r#"root     ::= answer | escalate
answer   ::= [^{ \t\r\n] [^\x00]*
escalate ::= "{\"glance\":\"escalate\",\"v\":1,\"reason\":\"" reason "\"}"
reason   ::= "reasoning" | "detail" | "code_math" | "uncertain" | "user_asked"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn run(pieces: &[&str]) -> (String, Decision) {
        let mut gate = EscalationGate::new();
        let mut spoken = String::new();
        for piece in pieces {
            match gate.push(piece) {
                Step::Wait => {}
                Step::Speak(text) => spoken.push_str(&text),
                Step::Stop(decision) => return (spoken, decision),
            }
        }
        (spoken, gate.finish())
    }

    #[test]
    fn answers_flow_through_from_the_first_character() {
        let mut gate = EscalationGate::new();
        assert_eq!(gate.push("It"), Step::Speak("It".into()));
        assert_eq!(gate.push(" is."), Step::Speak(" is.".into()));
        assert_eq!(gate.finish(), Decision::Answer);
    }

    #[test]
    fn leading_whitespace_waits_for_the_first_real_character() {
        assert_eq!(
            run(&[" ", "\n", "Yes."]),
            (" \nYes.".into(), Decision::Answer)
        );
    }

    #[test]
    fn a_valid_signal_escalates_even_when_split_across_deltas() {
        let (spoken, decision) = run(&[
            " {\"glance\":",
            "\"escalate\",\"v\":1,",
            "\"reason\":\"code_math\"}",
        ]);
        assert_eq!(spoken, "");
        assert_eq!(decision, Decision::Escalate(Reason::CodeMath));
    }

    #[test]
    fn anything_else_that_starts_with_a_brace_is_invalid_and_silent() {
        let bad = [
            r#"{"glance":"escalate","v":1,"reason":"because"}"#,
            r#"{"glance":"escalate","v":2,"reason":"detail"}"#,
            r#"{"glance":"escalate","v":1.0,"reason":"detail"}"#,
            r#"{"glance":"escalate","v":"1","reason":"detail"}"#,
            r#"{"glance":"Escalate","v":1,"reason":"detail"}"#,
            r#"{"glance":"escalate","v":1,"reason":"detail","say":"hi"}"#,
            r#"{"glance":"escalate","v":1}"#,
            r#"{"answer":"four"}"#,
            "{not json}",
        ];
        for text in bad {
            assert_eq!(run(&[text]), (String::new(), Decision::Invalid), "{text}");
        }
    }

    #[test]
    fn an_unclosed_or_overlong_brace_is_invalid() {
        assert_eq!(run(&["{\"glance\":\"esc"]).1, Decision::Invalid);
        let long = format!("{{\"glance\":\"{}", "x".repeat(MAX_SIGNAL_BYTES));
        assert_eq!(run(&[&long]).1, Decision::Invalid);
    }

    #[test]
    fn text_after_the_object_is_never_spoken() {
        let (spoken, decision) =
            run(&[r#"{"glance":"escalate","v":1,"reason":"detail"} Also, the answer is 4."#]);
        assert_eq!(spoken, "");
        assert_eq!(decision, Decision::Escalate(Reason::Detail));
    }

    #[test]
    fn an_empty_reply_is_an_empty_answer() {
        assert_eq!(run(&[]), (String::new(), Decision::Answer));
        assert_eq!(run(&["  "]), (String::new(), Decision::Answer));
    }
}
