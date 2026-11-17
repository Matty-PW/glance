//! The conversation about the current screenshot (`docs/DESIGN.md` §4). In
//! memory only: it is dropped on "New session", after the idle timeout, and
//! at quit.
//!
//! Message layout, image first so the server can reuse its cached prefix:
//! `[system, user:[image, q1], assistant:a1, user:q2, assistant:a2, …]`.
//! When `look` brings a newer screenshot, the older image is replaced by a
//! short text placeholder and the new image goes into the new question's
//! message. Only one image is ever sent.

use std::fmt;

use glance_llm_client::{Message, Part};
use tokio::time::Instant;

/// Stands in for a screenshot that a newer one has replaced.
pub(crate) const EARLIER_SCREENSHOT: &str =
    "(An earlier screenshot was shown here; a newer one has replaced it.)";

/// A screenshot as sent to the model.
#[derive(Clone)]
pub(crate) struct Shot {
    pub(crate) image: Part,
    pub(crate) taken: Instant,
}

/// What an answered turn adds to the session.
pub(crate) struct Commit {
    pub(crate) question: String,
    pub(crate) answer: String,
    /// The screenshot this turn captured, if it captured one.
    pub(crate) shot: Option<Shot>,
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

    /// The messages that come before the new question (without the system
    /// prompt). `fresh_image`: the new question brings its own screenshot,
    /// so the current one is shown as a placeholder.
    pub(crate) fn history(&self, fresh_image: bool) -> Vec<Message> {
        let mut messages = Vec::with_capacity(self.exchanges.len() * 2);
        for exchange in &self.exchanges {
            let mut parts = Vec::with_capacity(2);
            match (exchange.image, &self.shot) {
                (Image::Current, Some(shot)) if !fresh_image => parts.push(shot.image.clone()),
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
            taken: Instant::now(),
        }
    }

    fn commit(q: &str, shot: Option<Shot>) -> Commit {
        Commit {
            question: q.into(),
            answer: format!("answer to {q}"),
            shot,
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
            layout(&s.history(false)),
            ["U:img1+q1", "A:answer to q1", "U:q2", "A:answer to q2"]
        );
    }

    #[test]
    fn a_newer_screenshot_replaces_the_old_one_with_a_placeholder() {
        let mut s = Session::default();
        s.commit(commit("q1", Some(shot(1))), 12);
        // Asking with a fresh image: the current one is not sent again.
        assert_eq!(layout(&s.history(true)), ["U:earlier+q1", "A:answer to q1"]);
        s.commit(commit("q2", Some(shot(2))), 12);
        s.commit(commit("q3", None), 12);
        assert_eq!(
            layout(&s.history(false)),
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
            layout(&s.history(false)),
            ["U:img1+q2", "A:answer to q2", "U:q3", "A:answer to q3"]
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
