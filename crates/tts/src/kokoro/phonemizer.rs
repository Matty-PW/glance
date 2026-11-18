//! Text to Kokoro phonemes.
//!
//! [`Misaki`] wraps `misaki-rs` with its `espeak` feature off (no GPL code;
//! see `THIRD_PARTY.md` for the caveat about its dictionary data). Words its
//! lexicon does not know come back to [`OovFallback`], glance's own small
//! fallback: clock times, dotted and slashed names, compounds of two known
//! words, then simple letter-to-sound rules. The trait exists so a different
//! G2P (CMUdict, `docs/DESIGN.md` §6.3) can replace misaki-rs later.

use misaki_rs::fallback::FallbackError;
use misaki_rs::{Fallback, G2P, Language};

use crate::TtsError;

/// Turns one sentence of text into a phoneme string in Kokoro's alphabet.
pub trait Phonemizer: Send {
    fn phonemize(&self, text: &str) -> Result<String, TtsError>;
}

/// American or British English.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accent {
    American,
    British,
}

impl Accent {
    fn language(self) -> Language {
        match self {
            Accent::American => Language::EnglishUS,
            Accent::British => Language::EnglishGB,
        }
    }

    /// Kokoro voice ids start with `a` (American) or `b` (British).
    pub fn of_voice(voice_id: &str) -> Self {
        if voice_id.starts_with('b') {
            Accent::British
        } else {
            Accent::American
        }
    }
}

pub struct Misaki {
    g2p: G2P,
}

impl Misaki {
    /// Builds the lexicon and tagger (about 0.2 s; do it once).
    pub fn new(accent: Accent) -> Self {
        let language = accent.language();
        Self {
            g2p: G2P::with_fallback(language, Some(Box::new(OovFallback::new(language)))),
        }
    }
}

impl Phonemizer for Misaki {
    fn phonemize(&self, text: &str) -> Result<String, TtsError> {
        let phonemes = match self.g2p.g2p(text) {
            Ok((phonemes, _)) => phonemes,
            // A sentence is never dropped for its punctuation: retry with
            // everything but letters, digits and sentence marks blanked.
            Err(_) => {
                let plain: String = text
                    .chars()
                    .map(|c| {
                        if c.is_alphanumeric() || " .,!?;:'-".contains(c) {
                            c
                        } else {
                            ' '
                        }
                    })
                    .collect();
                self.g2p
                    .g2p(&plain)
                    .map_err(|e| TtsError::Engine(format!("phonemizer: {e}")))?
                    .0
            }
        };
        // Anything still unknown is dropped rather than spoken as a marker.
        Ok(phonemes.replace(&self.g2p.unk, "").trim().to_owned())
    }
}

/// glance's fallback for words misaki's lexicon does not resolve.
pub struct OovFallback {
    base: G2P,
}

impl OovFallback {
    pub fn new(language: Language) -> Self {
        Self {
            base: G2P::new(language),
        }
    }

    /// Phonemes for text the base lexicon fully knows, else `None`.
    fn known(&self, text: &str) -> Option<String> {
        let (phonemes, _) = self.base.g2p(text).ok()?;
        let phonemes = phonemes.trim().to_owned();
        (!phonemes.is_empty() && !phonemes.contains(&self.base.unk)).then_some(phonemes)
    }

    fn spell(&self, letters: &str) -> String {
        letters
            .chars()
            .filter_map(|c| self.known(&c.to_uppercase().to_string()))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn word(&self, word: &str) -> String {
        if let Some(p) = self.known(word) {
            return p;
        }
        let lower = word.to_lowercase();
        let letters_only = lower.chars().all(|c| c.is_ascii_lowercase());
        let no_vowel = !lower.chars().any(|c| "aeiouy".contains(c));
        if !letters_only || (lower.len() <= 3 && no_vowel) {
            return self.spell(&lower);
        }
        // A compound of two known words, longest left part first
        // ("signups", "gmail").
        for split in (1..lower.len()).rev() {
            let (left, right) = lower.split_at(split);
            let one_letter_word = left.len() == 1 && "abcdefgjkpqtvxz".contains(left);
            if right.len() < 3 || left.len() == 2 || (left.len() == 1 && !one_letter_word) {
                continue;
            }
            let left = if left.len() == 1 {
                self.known(&left.to_uppercase())
            } else {
                self.known(left)
            };
            if let (Some(a), Some(b)) = (left, self.known(right)) {
                return format!("{a}{b}");
            }
        }
        letter_to_sound(&lower)
    }
}

impl Fallback for OovFallback {
    fn phonemize(&self, word: &str) -> Result<String, FallbackError> {
        // misaki hands over whole merged tokens ("86%", "Harbor-5G.") as soon
        // as one part is unresolved, including parts its own rules can finish.
        if let Some(p) = self.known(word) {
            return Ok(p);
        }
        // Keep sentence punctuation that is glued to the token.
        let core = word.trim_end_matches(['.', ',', '!', '?', ';', ':']);
        let tail = &word[core.len()..];
        if !tail.is_empty() && !core.is_empty() {
            return self.phonemize(core).map(|p| format!("{p}{tail}"));
        }
        if let Some(p) = self.clock_time(word) {
            return Ok(p);
        }
        // Symbols on their own ("==", "->", "&&"): name the common ones,
        // skip the rest rather than fail the sentence.
        if !word.chars().any(char::is_alphanumeric) {
            let spoken: Vec<String> = word
                .chars()
                .filter_map(|c| {
                    let name = match c {
                        '=' => "equals",
                        '+' => "plus",
                        '<' => "less than",
                        '>' => "greater than",
                        '&' => "and",
                        '%' => "percent",
                        _ => return None,
                    };
                    self.known(name)
                })
                .collect();
            // "==" is said once, not "equals equals".
            let mut spoken = spoken;
            spoken.dedup();
            return Ok(spoken.join(" "));
        }
        // Names with separators: file.rs, a/b/c, snake_case, kebab-case.
        let mut out: Vec<String> = Vec::new();
        let mut piece = String::new();
        let flush = |piece: &mut String, out: &mut Vec<String>| {
            if !piece.is_empty() {
                out.push(self.word(piece));
                piece.clear();
            }
        };
        for c in word.chars() {
            match c {
                '.' => {
                    flush(&mut piece, &mut out);
                    out.push(self.known("dot").unwrap_or_default());
                }
                '/' => {
                    flush(&mut piece, &mut out);
                    out.push(self.known("slash").unwrap_or_default());
                }
                c if c.is_alphanumeric() => {
                    // Split letters from digits: "u64" is "u", "64".
                    if piece
                        .chars()
                        .last()
                        .is_some_and(|p| p.is_ascii_digit() != c.is_ascii_digit())
                    {
                        flush(&mut piece, &mut out);
                    }
                    piece.push(c);
                }
                _ => flush(&mut piece, &mut out),
            }
        }
        flush(&mut piece, &mut out);
        let joined = out
            .into_iter()
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if joined.is_empty() {
            Err(FallbackError::NoPhonemes {
                word: word.to_owned(),
            })
        } else {
            Ok(joined)
        }
    }
}

impl OovFallback {
    /// 2:30 is "two thirty", 9:05 "nine oh five", 3:00 "three o'clock".
    fn clock_time(&self, word: &str) -> Option<String> {
        let (h, m) = word.split_once(':')?;
        let digits = h.chars().chain(m.chars()).all(|c| c.is_ascii_digit());
        if !(1..=2).contains(&h.len()) || m.len() != 2 || !digits {
            return None;
        }
        let minutes = match m {
            "00" => "o'clock".to_owned(),
            m if m.starts_with('0') => format!("oh {}", &m[1..]),
            m => m.to_owned(),
        };
        self.known(&format!("{h} {minutes}"))
    }
}

/// A very small English letter-to-sound rule set, in misaki/Kokoro phoneme
/// symbols. Good enough to make an unknown name recognisable; it will not
/// always be right.
pub fn letter_to_sound(word: &str) -> String {
    const RULES: &[(&str, &str)] = &[
        ("tion", "ʃən"),
        ("sion", "ʒən"),
        ("eigh", "A"),
        ("ough", "O"),
        ("igh", "I"),
        ("tch", "ʧ"),
        ("dge", "ʤ"),
        ("sch", "sk"),
        ("ch", "ʧ"),
        ("sh", "ʃ"),
        ("th", "θ"),
        ("ph", "f"),
        ("wh", "w"),
        ("ck", "k"),
        ("ng", "ŋ"),
        ("qu", "kw"),
        ("ee", "i"),
        ("ea", "i"),
        ("oo", "u"),
        ("ou", "W"),
        ("ow", "O"),
        ("oi", "Y"),
        ("oy", "Y"),
        ("ai", "A"),
        ("ay", "A"),
        ("au", "ɔ"),
        ("aw", "ɔ"),
        ("oa", "O"),
        ("ue", "u"),
        ("ui", "u"),
        ("eu", "ju"),
        ("ew", "ju"),
        ("ar", "ɑɹ"),
        ("er", "əɹ"),
        ("ir", "ɜɹ"),
        ("ur", "ɜɹ"),
        ("or", "ɔɹ"),
    ];
    const VOWEL_PHONEMES: &str = "AIOWYiuæɑɔəɛɪʊʌɜ";
    let chars: Vec<char> = word.chars().collect();
    let is_vowel = |c: char| "aeiou".contains(c);
    let n = chars.len();
    let mut out = String::new();
    let mut i = 0;
    while i < n {
        let rest: String = chars[i..].iter().collect();
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        let after = chars.get(i + 2).copied();
        if i == 0 && rest.starts_with("kn") {
            out.push('n');
            i += 2;
            continue;
        }
        if i == 0 && rest.starts_with("wr") {
            out.push('ɹ');
            i += 2;
            continue;
        }
        if let Some((pattern, phonemes)) = RULES.iter().find(|(p, _)| rest.starts_with(p)) {
            // "er"/"ar"/"or" before a vowel is a consonant r, not an
            // r-coloured vowel.
            let r_then_vowel = pattern.len() == 2
                && pattern.ends_with('r')
                && chars.get(i + 2).is_some_and(|&c| is_vowel(c));
            if !r_then_vowel {
                out.push_str(phonemes);
                i += pattern.chars().count();
                continue;
            }
        }
        // A doubled consonant is said once.
        if !is_vowel(c) && next == Some(c) {
            i += 1;
            continue;
        }
        let last = i + 1 == n;
        // Vowel + one consonant + vowel: the vowel is "long" (open syllable,
        // magic e).
        let open = next.is_some_and(|x| !is_vowel(x) && x != 'x')
            && after.is_some_and(|x| is_vowel(x) || x == 'y');
        let has_vowel_sound = |out: &str| out.chars().any(|p| VOWEL_PHONEMES.contains(p));
        let phoneme: &str = match c {
            'a' if last => "ə",
            'a' if open => "A",
            'a' => "æ",
            'e' if last && has_vowel_sound(&out) => "",
            'e' if last || open => "i",
            'e' => "ɛ",
            'i' if next.is_some_and(|x| is_vowel(x) || x == 'y') || last => "i",
            'i' if open => "I",
            'i' => "ɪ",
            'o' if last || open => "O",
            'o' => "ɑ",
            'u' if open => "u",
            'u' => "ʌ",
            'y' if i == 0 => "j",
            'y' if last && !has_vowel_sound(&out) => "I",
            'y' if last => "i",
            'y' if next.is_some_and(is_vowel) => "j",
            'y' => "ɪ",
            'c' if next.is_some_and(|x| "eiy".contains(x)) => "s",
            'c' | 'q' => "k",
            'g' if last || next != Some('e') || i == 0 => "ɡ",
            'g' => "ʤ",
            'j' => "ʤ",
            'x' => "ks",
            'r' => "ɹ",
            'b' => "b",
            'd' => "d",
            'f' => "f",
            'h' => "h",
            'k' => "k",
            'l' => "l",
            'm' => "m",
            'n' => "n",
            'p' => "p",
            's' => "s",
            't' => "t",
            'v' => "v",
            'w' => "w",
            'z' => "z",
            _ => "",
        };
        out.push_str(phoneme);
        i += 1;
    }
    // Primary stress on the first vowel.
    match out
        .char_indices()
        .find(|(_, p)| VOWEL_PHONEMES.contains(*p))
    {
        Some((at, _)) => format!("{}ˈ{}", &out[..at], &out[at..]),
        None => out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn us() -> Misaki {
        Misaki::new(Accent::American)
    }

    #[test]
    fn plain_sentences_use_the_lexicon() {
        let p = us().phonemize("Please read the report.").unwrap();
        assert_eq!(p, "plˈiz ɹˈid ðə ɹəpˈɔɹt.");
    }

    #[test]
    fn unknown_names_get_a_reading_instead_of_a_marker() {
        let p = us().phonemize("Priya merged it.").unwrap();
        assert!(p.starts_with("pɹˈijə"), "{p}");
        assert!(!p.contains('❓'), "{p}");
    }

    #[test]
    fn clock_times_and_file_names() {
        let ph = us();
        let p = ph.phonemize("Lunch is at 12:15 in manifest.rs.").unwrap();
        assert!(p.contains("twˈɛlv fˌɪftˈin"), "{p}");
        assert!(p.contains("dˈɑt ˈɑɹ ˈɛs"), "{p}");
    }

    #[test]
    fn symbols_never_fail_a_sentence() {
        let ph = us();
        let p = ph
            .phonemize("An assertion left == right failed with left: 3 and right: 4.")
            .unwrap();
        assert!(p.contains("ˈikw"), "{p}"); // "equals"
        for text in ["It returns -> Result.", "Use && here.", "See ~~~ below.", "x |> y"] {
            assert!(!ph.phonemize(text).unwrap().is_empty(), "{text}");
        }
    }

    #[test]
    fn accent_follows_the_voice_id() {
        assert_eq!(Accent::of_voice("bf_emma"), Accent::British);
        assert_eq!(Accent::of_voice("af_heart"), Accent::American);
    }

    #[test]
    fn letter_to_sound_stresses_the_first_vowel() {
        assert_eq!(letter_to_sound("blik"), "blˈɪk");
        assert_eq!(letter_to_sound("knight"), "nˈIt");
    }
}
