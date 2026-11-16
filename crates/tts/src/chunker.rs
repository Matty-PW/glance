//! Turns a stream of model tokens into chunks that are ready to be spoken.
//!
//! A chunk is normally one sentence. The first chunk of a reply may end early
//! at a clause boundary so speech can start sooner, and a run-on with no
//! punctuation is cut at a word boundary so it cannot stall speech forever.

use crate::speakable::speakable;

/// Abbreviations whose trailing full stop does not end a sentence.
const ABBREVIATIONS: &[&str] = &[
    "mr", "mrs", "ms", "dr", "prof", "sr", "jr", "st", "vs", "etc", "e.g", "i.e", "fig", "approx",
    "inc", "ltd", "co",
];

#[derive(Debug, Clone, Copy)]
pub struct ChunkerConfig {
    /// The first chunk may end at a comma, semicolon, colon or dash once it
    /// is at least this many characters long. 0 disables early chunks.
    pub early_first_chunk_chars: usize,
    /// A chunk with no usable punctuation is cut at the last space before
    /// this many characters.
    pub max_chunk_chars: usize,
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        Self {
            early_first_chunk_chars: 40,
            max_chunk_chars: 280,
        }
    }
}

#[derive(Debug)]
pub struct SentenceChunker {
    config: ChunkerConfig,
    buffer: String,
    emitted_any: bool,
}

impl SentenceChunker {
    pub fn new(config: ChunkerConfig) -> Self {
        Self {
            config,
            buffer: String::new(),
            emitted_any: false,
        }
    }

    /// Feed the next piece of streamed text. Returns every chunk that is now
    /// complete, cleaned up for speech. Chunks with nothing speakable in them
    /// are dropped.
    pub fn push(&mut self, delta: &str) -> Vec<String> {
        self.buffer.push_str(delta);
        let mut chunks = Vec::new();
        while let Some(end) = self.next_boundary() {
            let raw: String = self.buffer.drain(..end).collect();
            self.emit(&raw, &mut chunks);
        }
        chunks
    }

    /// The stream has ended: return whatever is left.
    pub fn finish(&mut self) -> Option<String> {
        let raw = std::mem::take(&mut self.buffer);
        let mut chunks = Vec::new();
        self.emit(&raw, &mut chunks);
        chunks.pop()
    }

    fn emit(&mut self, raw: &str, chunks: &mut Vec<String>) {
        let text = speakable(raw);
        if !text.is_empty() {
            self.emitted_any = true;
            chunks.push(text);
        }
    }

    /// Byte offset just past the next complete chunk, if there is one.
    fn next_boundary(&self) -> Option<usize> {
        let text = self.buffer.as_str();
        if let Some(end) = sentence_end(text) {
            return Some(end);
        }
        if !self.emitted_any
            && self.config.early_first_chunk_chars > 0
            && let Some(end) = clause_end(text, self.config.early_first_chunk_chars)
        {
            return Some(end);
        }
        if text.chars().count() > self.config.max_chunk_chars {
            let limit = text
                .char_indices()
                .nth(self.config.max_chunk_chars)
                .map_or(text.len(), |(i, _)| i);
            return text[..limit].rfind(char::is_whitespace).map(|i| i + 1);
        }
        None
    }
}

/// First byte offset where a sentence has definitely ended.
///
/// A line break always ends a chunk. A `.`, `!`, `?` or `…` ends one when it is
/// followed by whitespace and then more text, so the decision can look at
/// what comes next (which costs one token of delay and avoids splitting
/// "3.14", "e.g. this" or "Dr. Smith").
fn sentence_end(text: &str) -> Option<usize> {
    let mut chars = text.char_indices().peekable();
    let mut seen_text = false;
    while let Some((i, c)) = chars.next() {
        if c == '\n' {
            if seen_text {
                return Some(i + 1);
            }
            continue;
        }
        if !c.is_whitespace() {
            seen_text = true;
        }
        if !matches!(c, '.' | '!' | '?' | '…') {
            continue;
        }
        // Swallow repeated terminators and closing quotes or brackets.
        let mut end = i + c.len_utf8();
        while let Some(&(j, next)) = chars.peek() {
            if matches!(
                next,
                '.' | '!' | '?' | '…' | '"' | '\'' | ')' | ']' | '”' | '’'
            ) {
                end = j + next.len_utf8();
                chars.next();
            } else {
                break;
            }
        }
        let rest = &text[end..];
        let Some(first) = rest.chars().next() else {
            return None; // nothing after it yet: wait for more text
        };
        if !first.is_whitespace() {
            continue; // "3.14", "manifest.rs", "v1.2"
        }
        if first == '\n' {
            return Some(end + 1);
        }
        let Some(next_word) = rest.trim_start().chars().next() else {
            return None; // only whitespace so far: wait
        };
        if c == '.' && (next_word.is_lowercase() || is_abbreviation(&text[..i])) {
            continue;
        }
        return Some(end + (rest.len() - rest.trim_start().len()));
    }
    None
}

/// True when the text before a full stop ends in something that is not a
/// sentence: a known abbreviation, a single initial, or a list number.
fn is_abbreviation(before: &str) -> bool {
    let word = before
        .rsplit(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or("");
    let lower = word.to_lowercase();
    if ABBREVIATIONS.contains(&lower.as_str()) {
        return true;
    }
    let mut letters = word.chars();
    let single_initial =
        matches!((letters.next(), letters.next()), (Some(c), None) if c.is_uppercase());
    let list_number = !word.is_empty()
        && word.len() <= 2
        && word.chars().all(|c| c.is_ascii_digit())
        && before[..before.len() - word.len()].trim().is_empty();
    single_initial || list_number
}

/// Offset just past the first clause break (`,` `;` `:` or a dash, followed
/// by whitespace and more text) that leaves at least `min_chars` before it.
fn clause_end(text: &str, min_chars: usize) -> Option<usize> {
    let mut count = 0;
    let chars = text.char_indices().peekable();
    for (i, c) in chars {
        count += 1;
        if count < min_chars || !matches!(c, ',' | ';' | ':' | '—' | '–') {
            continue;
        }
        let end = i + c.len_utf8();
        let rest = &text[end..];
        if rest.starts_with(char::is_whitespace) && !rest.trim_start().is_empty() {
            return Some(end + (rest.len() - rest.trim_start().len()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk_all(deltas: &[&str], config: ChunkerConfig) -> Vec<String> {
        let mut chunker = SentenceChunker::new(config);
        let mut out = Vec::new();
        for delta in deltas {
            out.extend(chunker.push(delta));
        }
        out.extend(chunker.finish());
        out
    }

    fn no_early() -> ChunkerConfig {
        ChunkerConfig {
            early_first_chunk_chars: 0,
            ..Default::default()
        }
    }

    /// Feed the text one character at a time, the worst case for streaming.
    fn by_char(text: &str, config: ChunkerConfig) -> Vec<String> {
        let pieces: Vec<String> = text.chars().map(String::from).collect();
        let refs: Vec<&str> = pieces.iter().map(String::as_str).collect();
        chunk_all(&refs, config)
    }

    #[test]
    fn splits_sentences_as_they_complete() {
        let mut chunker = SentenceChunker::new(no_early());
        assert!(
            chunker.push("The error is on line 42.").is_empty(),
            "must wait to see what follows"
        );
        assert_eq!(chunker.push(" It"), ["The error is on line 42."]);
        assert!(chunker.push(" expects a number").is_empty());
        assert_eq!(chunker.finish().as_deref(), Some("It expects a number"));
        assert_eq!(chunker.finish(), None);
    }

    #[test]
    fn same_result_however_the_stream_is_cut() {
        let text = "Week 4 had the most users, with 1,720. That is up from 1,310! Is that what you wanted?";
        let expected = [
            "Week 4 had the most users, with 1,720.",
            "That is up from 1,310!",
            "Is that what you wanted?",
        ];
        assert_eq!(by_char(text, no_early()), expected);
        assert_eq!(chunk_all(&[text], no_early()), expected);
    }

    #[test]
    fn does_not_split_numbers_filenames_or_abbreviations() {
        for text in [
            "The total is 3.14 percent of budget.",
            "The file manifest.rs has an error.",
            "Ask Dr. Smith about it.",
            "It lists tabs, e.g. store and download.",
            "Version 1.2 is installed, i.e. the latest.",
            "Signed by J. Okafor today.",
        ] {
            assert_eq!(by_char(text, no_early()), [text], "{text}");
        }
    }

    #[test]
    fn line_breaks_and_list_numbers() {
        let text = "Two things:\n1. Fix the type.\n2. Rerun the tests.";
        assert_eq!(
            by_char(text, no_early()),
            ["Two things:", "Fix the type.", "Rerun the tests."]
        );
    }

    #[test]
    fn closing_quotes_stay_with_their_sentence() {
        let text = "It says \"expected u64.\" That is the cause.";
        assert_eq!(
            by_char(text, no_early()),
            ["It says \"expected u64.\"", "That is the cause."]
        );
    }

    #[test]
    fn first_chunk_may_end_early_at_a_clause() {
        let text = "The design review moved to Thursday at 2:30 pm, and it is now in room Juniper. Send slides to Tomas.";
        let chunks = by_char(text, ChunkerConfig::default());
        assert_eq!(
            chunks,
            [
                "The design review moved to Thursday at 2:30 pm,",
                "and it is now in room Juniper.",
                "Send slides to Tomas.",
            ]
        );
        // Only the first chunk: later sentences are never split at commas.
        let later =
            "Yes. The design review moved to Thursday at 2:30 pm, and it is in room Juniper.";
        assert_eq!(by_char(later, ChunkerConfig::default()).len(), 2);
        // A short first sentence is left alone.
        assert_eq!(
            by_char("Yes, it passed.", ChunkerConfig::default()),
            ["Yes, it passed."]
        );
    }

    #[test]
    fn run_ons_are_cut_at_a_word_boundary() {
        let config = ChunkerConfig {
            early_first_chunk_chars: 0,
            max_chunk_chars: 30,
        };
        let text = "one two three four five six seven eight nine ten eleven twelve";
        let chunks = by_char(text, config);
        assert!(chunks.len() >= 2);
        assert!(chunks.iter().all(|c| c.chars().count() <= 30), "{chunks:?}");
        assert_eq!(chunks.join(" "), text);
    }

    #[test]
    fn unspeakable_chunks_are_dropped() {
        assert_eq!(by_char("...\n---\n**\nDone.", no_early()), ["Done."]);
        assert!(by_char("   \n ", no_early()).is_empty());
    }

    #[test]
    fn handles_multibyte_text() {
        let text = "Das Café öffnet um 9 Uhr… Bitte später wiederkommen — danke!";
        assert_eq!(
            by_char(text, no_early()),
            [
                "Das Café öffnet um 9 Uhr…",
                "Bitte später wiederkommen — danke!"
            ]
        );
    }
}
