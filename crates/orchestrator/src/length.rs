//! How long spoken answers are (`response_length`, one setting for FAST and
//! DEEP). The prompt alone did not keep answers short (M4 measurements:
//! a "two or three sentences" prompt left them as long and cost accuracy),
//! so the cap is enforced in code: speech stops at a sentence boundary once
//! the cap is reached, generation is cancelled, and only the spoken part is
//! kept in the session, so "tell me more" continues from what was heard.
//!
//! - `short`: a brief hint in the prompt and a hard cap of 2 sentences.
//! - `medium` (default): the original prompt, a safety cap of 8 sentences.
//! - `long`: a detail nudge, no cap, three times the token limit.
//!
//! An explicit request ("walk me through", "in detail", a list) lifts the cap
//! and the token limit in every mode.

/// Sentences spoken in `short` and `medium` mode.
pub const SHORT_CAP: usize = 2;
pub const MEDIUM_CAP: usize = 8;
/// Token limit multiplier for `long` and for explicit detail requests.
const LONG_TOKENS: u32 = 3;

/// What "tell me more" turns into for the model.
pub const CONTINUE_PROMPT: &str = "Continue your previous answer from exactly where it stopped. \
Do not repeat anything you already said.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResponseLength {
    Short,
    #[default]
    Medium,
    Long,
}

impl ResponseLength {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "short" => Some(Self::Short),
            "medium" => Some(Self::Medium),
            "long" => Some(Self::Long),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Short => "short",
            Self::Medium => "medium",
            Self::Long => "long",
        }
    }

    /// Appended to the system prompt; `medium` keeps the original wording.
    pub fn hint(self) -> Option<&'static str> {
        match self {
            Self::Short => Some(" Keep it to one or two short sentences."),
            Self::Medium => None,
            Self::Long => Some(
                " The user wants detail: give a complete answer, still in plain spoken sentences.",
            ),
        }
    }

    /// Sentences to speak for this question, or `None` for no cap.
    pub fn sentence_cap(self, question: &str) -> Option<usize> {
        if wants_detail(question) {
            return None;
        }
        match self {
            Self::Short => Some(SHORT_CAP),
            Self::Medium => Some(MEDIUM_CAP),
            Self::Long => None,
        }
    }

    /// The token limit for this question, from the tier's normal one.
    pub fn max_tokens(self, base: u32, question: &str) -> u32 {
        if self == Self::Long || wants_detail(question) {
            base.saturating_mul(LONG_TOKENS)
        } else {
            base
        }
    }
}

/// The question explicitly asks for a walkthrough, a list or detail.
pub fn wants_detail(question: &str) -> bool {
    const PHRASES: &[&str] = &[
        "walk me through",
        "walk through",
        "step by step",
        "in detail",
        "detailed",
        "go through",
        "explain each",
        "list all",
        "list the",
        "list every",
        "make a list",
        "give me a list",
        "all the details",
        "tell me everything",
    ];
    let q = normalize(question);
    PHRASES.iter().any(|p| contains_words(&q, p))
}

/// "Tell me more", "go on" and the like, said on their own: continue the
/// last answer from where it stopped.
pub fn is_more(question: &str) -> bool {
    const PHRASES: &[&str] = &[
        "tell me more",
        "go on",
        "more",
        "continue",
        "keep going",
        "and then",
        "what else",
        "carry on",
    ];
    let q = normalize(question);
    let q = q
        .strip_prefix("please ")
        .unwrap_or(&q)
        .trim_end_matches(" please");
    PHRASES.contains(&q)
}

/// The chunk ends a sentence (the chunker may also emit an early clause,
/// which does not count).
pub fn ends_sentence(chunk: &str) -> bool {
    chunk
        .trim_end()
        .trim_end_matches(['"', '\'', ')', '”', '’'])
        .ends_with(['.', '!', '?'])
}

/// Lowercase, punctuation to spaces, single spaces.
fn normalize(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '\'' {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn contains_words(haystack: &str, phrase: &str) -> bool {
    format!(" {haystack} ").contains(&format!(" {phrase} "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_per_mode_and_the_explicit_override() {
        let q = "What does this toggle do?";
        assert_eq!(ResponseLength::Short.sentence_cap(q), Some(2));
        assert_eq!(ResponseLength::Medium.sentence_cap(q), Some(8));
        assert_eq!(ResponseLength::Long.sentence_cap(q), None);
        let walk = "Walk me through three different ways to fix this function.";
        assert_eq!(ResponseLength::Short.sentence_cap(walk), None);
        assert_eq!(ResponseLength::Short.max_tokens(200, walk), 600);
        assert_eq!(ResponseLength::Medium.max_tokens(200, q), 200);
        assert_eq!(ResponseLength::Long.max_tokens(200, q), 600);
        assert!(wants_detail("Explain in detail what each toggle does"));
        assert!(!wants_detail("Is the playlist public?"));
    }

    #[test]
    fn tell_me_more_is_recognized_only_on_its_own() {
        for q in [
            "Tell me more.",
            "go on",
            "Go on, please.",
            "Continue!",
            "more",
            "What else?",
        ] {
            assert!(is_more(q), "{q}");
        }
        for q in [
            "Tell me more about the chart",
            "Should I continue the download?",
            "",
        ] {
            assert!(!is_more(q), "{q}");
        }
    }

    #[test]
    fn only_full_sentences_count() {
        assert!(ends_sentence("It is a code editor."));
        assert!(ends_sentence("Really?"));
        assert!(ends_sentence("He said \"stop.\""));
        assert!(!ends_sentence(
            "The build failed because one test timed out,"
        ));
    }

    #[test]
    fn settings_parse() {
        assert_eq!(ResponseLength::parse("Short"), Some(ResponseLength::Short));
        assert_eq!(ResponseLength::parse("huge"), None);
        assert_eq!(ResponseLength::default(), ResponseLength::Medium);
    }
}
