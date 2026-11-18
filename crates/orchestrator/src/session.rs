//! The conversation about the current screenshot (`docs/DESIGN.md` §4). In
//! memory only: it is dropped on "New session", after the idle timeout, and
//! at quit.
//!
//! Message layout, image first so the server can reuse its cached prefix:
//! `[system, user:[image, q1], assistant:a1, user:q2, assistant:a2, …]`.
//! When `look` brings a newer screenshot, the older image is replaced by a
//! short text placeholder and the new image goes into the new question's
//! message. Only one image is ever sent.
//!
//! DEEP's layout differs: the image is a user message of its own, followed
//! by the question as a second user message. Its prewarm is sent as
//! `[system, …, user:image]` with no assistant turn opened, so it is an exact
//! prefix of the question's prompt. Hybrid models (Qwen3.8: attention plus
//! recurrent layers) can only resume cached state from the end of an
//! earlier prompt; with the image and question in one message the prewarm
//! was never reused (M4 bake-off: 105 of ~2,820 tokens).

use std::fmt;

use glance_llm_client::{Message, Part};
use tokio::time::Instant;

/// Stands in for a screenshot that a newer one has replaced.
pub(crate) const EARLIER_SCREENSHOT: &str =
    "(An earlier screenshot was shown here; a newer one has replaced it.)";

/// A screenshot as sent to the models: FAST gets a smaller copy than DEEP.
#[derive(Clone)]
pub(crate) struct Shot {
    pub(crate) image: Part,
    /// Present when a DEEP tier is configured.
    pub(crate) deep_image: Option<Part>,
    pub(crate) taken: Instant,
}

impl Shot {
    pub(crate) fn for_tier(&self, tier: Tier) -> &Part {
        match (tier, &self.deep_image) {
            (Tier::Deep, Some(deep)) => deep,
            _ => &self.image,
        }
    }
}

/// Which model a message list is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tier {
    Fast,
    Deep,
}

/// What an answered turn adds to the session.
pub(crate) struct Commit {
    pub(crate) question: String,
    pub(crate) answer: String,
    /// The screenshot this turn captured, if it captured one.
    pub(crate) shot: Option<Shot>,
    /// A "think harder" re-ask: DEEP's answer replaces the last answer
    /// instead of adding an exchange.
    pub(crate) replace_last: bool,
    /// DEEP gave this answer ("tell me more" then continues with DEEP).
    pub(crate) deep: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Image {
    /// This question was asked with the current screenshot attached.
    Current,
    /// It was asked with a screenshot that has since been replaced.
    Earlier,
    /// A follow-up: no screenshot of its own.
    None,
}

struct Exchange {
    question: String,
    answer: String,
    image: Image,
    deep: bool,
}

/// A summary for the app, with no content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionInfo {
    pub questions: usize,
    pub screenshot_taken: Option<std::time::Instant>,
}

#[derive(Default)]
pub(crate) struct Session {
    shot: Option<Shot>,
    exchanges: Vec<Exchange>,
}

impl fmt::Debug for Session {
    /// Counts only: questions and answers never reach a log.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Session({} questions, screenshot: {})",
            self.exchanges.len(),
            self.shot.is_some()
        )
    }
}

impl Session {
    pub(crate) fn is_empty(&self) -> bool {
        self.exchanges.is_empty()
    }

    pub(crate) fn has_screenshot(&self) -> bool {
        self.shot.is_some()
    }

    /// Whether DEEP gave the last answer; `None` before any answer.
    pub(crate) fn last_was_deep(&self) -> Option<bool> {
        self.exchanges.last().map(|e| e.deep)
    }

    /// The messages that come before the new question (without the system
    /// prompt). `fresh_image`: the new question brings its own screenshot,
    /// so the current one is shown as a placeholder.
    pub(crate) fn history(&self, fresh_image: bool, tier: Tier) -> Vec<Message> {
        self.render(&self.exchanges, fresh_image, tier)
    }

    /// For re-asking the last question: that question, and the history
    /// before it with the screenshot still in it. FAST's answer to it is
    /// left out, so DEEP answers afresh.
    pub(crate) fn reask(&self, tier: Tier) -> Option<(String, Vec<Message>)> {
        let (last, earlier) = self.exchanges.split_last()?;
        let mut messages = self.render(earlier, false, tier);
        if last.image == Image::Current
            && let Some(shot) = &self.shot
        {
            messages.push(Message::user(vec![shot.for_tier(tier).clone()]));
        }
        Some((last.question.clone(), messages))
    }

    fn render(&self, exchanges: &[Exchange], fresh_image: bool, tier: Tier) -> Vec<Message> {
        let mut messages = Vec::with_capacity(exchanges.len() * 2);
        for exchange in exchanges {
            let mut parts = Vec::with_capacity(2);
            match (exchange.image, &self.shot) {
                (Image::Current, Some(shot)) if !fresh_image && tier == Tier::Deep => {
                    messages.push(Message::user(vec![shot.for_tier(tier).clone()]));
                }
                (Image::Current, Some(shot)) if !fresh_image => {
                    parts.push(shot.for_tier(tier).clone());
                }
                (Image::Current | Image::Earlier, _) => {
                    parts.push(Part::Text(EARLIER_SCREENSHOT.into()));
                }
                (Image::None, _) => {}
            }
            parts.push(Part::Text(exchange.question.clone()));
            messages.push(Message::user(parts));
            messages.push(Message::assistant(exchange.answer.clone()));
        }
        messages
    }

    /// Add an answered turn, then drop the oldest exchanges beyond `max`.
    pub(crate) fn commit(&mut self, commit: Commit, max: usize) {
        if commit.replace_last
            && let Some(last) = self.exchanges.last_mut()
        {
            last.answer = commit.answer;
            last.deep = commit.deep;
            return;
        }
        let image = match commit.shot {
            Some(shot) => {
                for exchange in &mut self.exchanges {
                    if exchange.image == Image::Current {
                        exchange.image = Image::Earlier;
                    }
                }
                self.shot = Some(shot);
                Image::Current
            }
            None => Image::None,
        };
        self.exchanges.push(Exchange {
            question: commit.question,
            answer: commit.answer,
            image,
            deep: commit.deep,
        });
        while self.exchanges.len() > max.max(1) {
            let dropped = self.exchanges.remove(0);
            // The screenshot moves to the oldest remaining question, so it is
            // still sent first.
            if dropped.image == Image::Current
                && let Some(first) = self.exchanges.first_mut()
            {
                first.image = Image::Current;
            }
        }
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn info(&self) -> SessionInfo {
        SessionInfo {
            questions: self.exchanges.len(),
            screenshot_taken: self.shot.as_ref().map(|s| s.taken.into_std()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use glance_llm_client::Role;

    use super::*;

    fn shot(byte: u8) -> Shot {
        Shot {
            image: Part::Png(Arc::from(vec![byte])),
            deep_image: Some(Part::Png(Arc::from(vec![byte + 100]))),
            taken: Instant::now(),
        }
    }

    fn commit(q: &str, shot: Option<Shot>) -> Commit {
        Commit {
            question: q.into(),
            answer: format!("answer to {q}"),
            shot,
            replace_last: false,
            deep: false,
        }
    }

    /// Each message as a compact string: role, then its parts.
    fn layout(messages: &[Message]) -> Vec<String> {
        messages
            .iter()
            .map(|m| {
                let parts: Vec<String> = m
                    .parts
                    .iter()
                    .map(|p| match p {
                        Part::Png(b) => format!("img{}", b[0]),
                        Part::Text(t) if t == EARLIER_SCREENSHOT => "earlier".into(),
                        Part::Text(t) => t.clone(),
                    })
                    .collect();
                let role = if m.role == Role::User { "U" } else { "A" };
                format!("{role}:{}", parts.join("+"))
            })
            .collect()
    }

    #[test]
    fn follow_ups_keep_the_image_first() {
        let mut s = Session::default();
        s.commit(commit("q1", Some(shot(1))), 12);
        s.commit(commit("q2", None), 12);
        assert_eq!(
            layout(&s.history(false, Tier::Fast)),
            ["U:img1+q1", "A:answer to q1", "U:q2", "A:answer to q2"]
        );
    }

    #[test]
    fn a_newer_screenshot_replaces_the_old_one_with_a_placeholder() {
        let mut s = Session::default();
        s.commit(commit("q1", Some(shot(1))), 12);
        // Asking with a fresh image: the current one is not sent again.
        assert_eq!(
            layout(&s.history(true, Tier::Fast)),
            ["U:earlier+q1", "A:answer to q1"]
        );
        s.commit(commit("q2", Some(shot(2))), 12);
        s.commit(commit("q3", None), 12);
        assert_eq!(
            layout(&s.history(false, Tier::Fast)),
            [
                "U:earlier+q1",
                "A:answer to q1",
                "U:img2+q2",
                "A:answer to q2",
                "U:q3",
                "A:answer to q3"
            ]
        );
    }

    #[test]
    fn trimming_moves_the_screenshot_to_the_oldest_kept_question() {
        let mut s = Session::default();
        s.commit(commit("q1", Some(shot(1))), 2);
        s.commit(commit("q2", None), 2);
        s.commit(commit("q3", None), 2);
        assert_eq!(
            layout(&s.history(false, Tier::Fast)),
            ["U:img1+q2", "A:answer to q2", "U:q3", "A:answer to q3"]
        );
        assert_eq!(s.info().questions, 2);
    }

    #[test]
    fn deep_gets_its_own_larger_copy_of_the_screenshot_as_its_own_message() {
        let mut s = Session::default();
        s.commit(commit("q1", Some(shot(1))), 12);
        assert_eq!(
            layout(&s.history(false, Tier::Deep)),
            ["U:img101", "U:q1", "A:answer to q1"]
        );
        assert_eq!(layout(&s.history(false, Tier::Fast))[0], "U:img1+q1");
    }

    #[test]
    fn reask_leaves_out_the_last_answer_but_keeps_the_screenshot() {
        let mut s = Session::default();
        assert!(s.reask(Tier::Deep).is_none());
        s.commit(commit("q1", Some(shot(1))), 12);
        let (q, history) = s.reask(Tier::Deep).unwrap();
        assert_eq!(q, "q1");
        assert_eq!(layout(&history), ["U:img101"]);
        s.commit(commit("q2", None), 12);
        let (q, history) = s.reask(Tier::Deep).unwrap();
        assert_eq!(q, "q2");
        assert_eq!(layout(&history), ["U:img101", "U:q1", "A:answer to q1"]);
        // DEEP's answer then replaces FAST's.
        s.commit(
            Commit {
                question: "q2".into(),
                answer: "deep answer".into(),
                shot: None,
                replace_last: true,
                deep: true,
            },
            12,
        );
        assert_eq!(
            layout(&s.history(false, Tier::Deep)).last().unwrap(),
            "A:deep answer"
        );
        assert_eq!(s.info().questions, 2);
    }

    #[test]
    fn debug_shows_counts_only() {
        let mut s = Session::default();
        s.commit(commit("secret question", Some(shot(1))), 12);
        let shown = format!("{s:?}");
        assert_eq!(shown, "Session(1 questions, screenshot: true)");
        s.clear();
        assert!(s.is_empty() && !s.has_screenshot());
    }
}
