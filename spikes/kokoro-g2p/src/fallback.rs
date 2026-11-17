//! Out-of-vocabulary fallback without espeak: clock times, dotted/slashed
//! names, compound splitting, then simple letter-to-sound rules.
use misaki_rs::fallback::FallbackError;
use misaki_rs::{Fallback, G2P, Language};

pub struct OovFallback { base: G2P }

impl OovFallback {
    pub fn new(lang: Language) -> Self { Self { base: G2P::new(lang) } }

    /// Phonemes for text the base lexicon fully knows, else None.
    fn known(&self, text: &str) -> Option<String> {
        let (ph, _) = self.base.g2p(text).ok()?;
        let ph = ph.trim().to_string();
        (!ph.is_empty() && !ph.contains(&self.base.unk)).then_some(ph)
    }

    fn spell(&self, letters: &str) -> String {
        letters.chars().filter_map(|c| self.known(&c.to_uppercase().to_string())).collect::<Vec<_>>().join(" ")
    }

    fn word(&self, word: &str) -> String {
        if let Some(p) = self.known(word) { return p; }
        let lower = word.to_lowercase();
        let letters_only = lower.chars().all(|c| c.is_ascii_lowercase());
        if !letters_only || lower.len() <= 3 && !lower.chars().any(|c| "aeiouy".contains(c)) {
            return self.spell(&lower);
        }
        // Compound of two known words, longest left part first ("signups", "gmail").
        for split in (1..lower.len()).rev() {
            let (l, r) = lower.split_at(split);
            if r.len() < 3 || (l.len() == 1 && !"abcdefgjkpqtvxz".contains(l)) || (l.len() == 2) { continue; }
            let left = if l.len() == 1 { self.known(&l.to_uppercase()) } else { self.known(l) };
            if let (Some(a), Some(b)) = (left, self.known(r)) { return format!("{a}{b}"); }
        }
        letter_to_sound(&lower)
    }
}

impl Fallback for OovFallback {
    fn phonemize(&self, word: &str) -> Result<String, FallbackError> {
        // misaki hands over whole merged tokens ("86%", "Harbor-5G.") as soon as
        // one part is unresolved, including ones its own rules can finish.
        if let Some(p) = self.known(word) { return Ok(p); }
        // Keep sentence punctuation that is glued to the token.
        let core = word.trim_end_matches(['.', ',', '!', '?', ';', ':']);
        let tail = &word[core.len()..];
        if !tail.is_empty() && !core.is_empty() {
            return self.phonemize(core).map(|p| format!("{p}{tail}"));
        }
        // Clock times: 2:30 -> "two thirty", 9:05 -> "nine oh five", 3:00 -> "three o'clock".
        if let Some((h, m)) = word.split_once(':') {
            if (1..=2).contains(&h.len()) && m.len() == 2 && h.chars().chain(m.chars()).all(|c| c.is_ascii_digit()) {
                let minutes = match m { "00" => "o'clock".to_string(), m if m.starts_with('0') => format!("oh {}", &m[1..]), m => m.to_string() };
                if let Some(p) = self.known(&format!("{h} {minutes}")) { return Ok(p); }
            }
        }
        // Names with separators: file.rs, a/b/c, snake_case, kebab-case.
        let mut out: Vec<String> = Vec::new();
        let mut piece = String::new();
        let flush = |piece: &mut String, out: &mut Vec<String>| {
            if !piece.is_empty() { out.push(self.word(piece)); piece.clear(); }
        };
        for c in word.chars() {
            match c {
                '.' => { flush(&mut piece, &mut out); out.push(self.known("dot").unwrap_or_default()); }
                '/' => { flush(&mut piece, &mut out); out.push(self.known("slash").unwrap_or_default()); }
                '_' | '-' | ':' | '\\' => flush(&mut piece, &mut out),
                c if c.is_alphanumeric() => {
                    // split letters from digits: "u64" -> "u", "64"
                    if piece.chars().last().is_some_and(|p| p.is_ascii_digit() != c.is_ascii_digit()) { flush(&mut piece, &mut out); }
                    piece.push(c);
                }
                _ => flush(&mut piece, &mut out),
            }
        }
        flush(&mut piece, &mut out);
        let joined = out.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ");
        if joined.is_empty() { Err(FallbackError::NoPhonemes { word: word.to_string() }) } else { Ok(joined) }
    }
}

/// Very small English letter-to-sound rule set (misaki/Kokoro phoneme symbols).
/// Good enough to make an unknown name recognisable; it will not always be right.
pub fn letter_to_sound(word: &str) -> String {
    const RULES: &[(&str, &str)] = &[
        ("tion", "ʃən"), ("sion", "ʒən"), ("eigh", "A"), ("ough", "O"), ("igh", "I"), ("tch", "ʧ"), ("dge", "ʤ"),
        ("sch", "sk"), ("ch", "ʧ"), ("sh", "ʃ"), ("th", "θ"), ("ph", "f"), ("wh", "w"), ("ck", "k"), ("ng", "ŋ"),
        ("qu", "kw"), ("ee", "i"), ("ea", "i"), ("oo", "u"), ("ou", "W"), ("ow", "O"), ("oi", "Y"), ("oy", "Y"),
        ("ai", "A"), ("ay", "A"), ("au", "ɔ"), ("aw", "ɔ"), ("oa", "O"), ("ue", "u"), ("ui", "u"), ("eu", "ju"),
        ("ew", "ju"), ("ar", "ɑɹ"), ("er", "əɹ"), ("ir", "ɜɹ"), ("ur", "ɜɹ"), ("or", "ɔɹ"),
    ];
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
        if i == 0 && rest.starts_with("kn") { out.push('n'); i += 2; continue; }
        if i == 0 && rest.starts_with("wr") { out.push('ɹ'); i += 2; continue; }
        if let Some((pat, ph)) = RULES.iter().find(|(pat, _)| rest.starts_with(pat)) {
            // "er"/"ar"/"or" followed by a vowel is consonant r, not an r-coloured vowel
            let r_vowel = pat.len() == 2 && pat.ends_with('r') && chars.get(i + 2).is_some_and(|&c| is_vowel(c));
            if !r_vowel { out.push_str(ph); i += pat.chars().count(); continue; }
        }
        // doubled consonant: say it once
        if !is_vowel(c) && next == Some(c) { i += 1; continue; }
        let last = i + 1 == n;
        // vowel + single consonant + vowel: the vowel is "long" (open syllable / magic e)
        let open = next.is_some_and(|x| !is_vowel(x) && x != 'x') && after.is_some_and(|x| is_vowel(x) || x == 'y');
        let ph: &str = match c {
            'a' if last => "ə",
            'a' if open => "A",
            'a' => "æ",
            'e' if last && out.chars().any(|p| "AIOWYiuæɑɔəɛɪʊʌ".contains(p)) => "",
            'e' if last => "i",
            'e' if open => "i",
            'e' => "ɛ",
            'i' if next.is_some_and(|x| is_vowel(x) || x == 'y') || last => "i",
            'i' if open => "I",
            'i' => "ɪ",
            'o' if last || open => "O",
            'o' => "ɑ",
            'u' if open => "u",
            'u' => "ʌ",
            'y' if i == 0 => "j",
            'y' if last && out.chars().filter(|p| "AIOWYiuæɑɔəɛɪʊʌ".contains(*p)).count() == 0 => "I",
            'y' if last => "i",
            'y' if next.is_some_and(is_vowel) => "j",
            'y' => "ɪ",
            'c' if next.is_some_and(|x| "eiy".contains(x)) => "s",
            'c' | 'q' => "k",
            'g' if last || !next.is_some_and(|x| "e".contains(x)) || i == 0 => "ɡ",
            'g' => "ʤ",
            'j' => "ʤ", 'x' => "ks", 'r' => "ɹ",
            'b' => "b", 'd' => "d", 'f' => "f", 'h' => "h", 'k' => "k", 'l' => "l", 'm' => "m", 'n' => "n",
            'p' => "p", 's' => "s", 't' => "t", 'v' => "v", 'w' => "w", 'z' => "z",
            _ => "",
        };
        out.push_str(ph);
        i += 1;
    }
    // primary stress on the first vowel
    match out.char_indices().find(|(_, p)| "AIOWYiuæɑɔəɛɪʊʌɜ".contains(*p)) {
        Some((at, _)) => format!("{}ˈ{}", &out[..at], &out[at..]),
        None => out,
    }
}
