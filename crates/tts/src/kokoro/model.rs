//! Kokoro-82M on ONNX Runtime: phoneme tokens + a style row + speed in,
//! 24 kHz mono samples out. Inputs per the model card: `input_ids` (at most
//! 510 tokens, padded with a 0 on each side), `style` (one 256-float row),
//! `speed`.

use std::collections::HashMap;
use std::path::Path;

use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;

use super::voice::VoiceStyle;
use crate::TtsError;

/// Kokoro's output rate.
pub const SAMPLE_RATE: u32 = 24_000;
/// Most phoneme tokens one inference accepts.
pub const MAX_TOKENS: usize = 510;

pub struct KokoroModel {
    session: Session,
    vocab: HashMap<char, i64>,
}

fn engine(context: &str) -> impl Fn(ort::Error) -> TtsError + '_ {
    move |e| TtsError::Engine(format!("{context}: {e}"))
}

impl KokoroModel {
    /// `onnx` and `tokenizer` must already be verified against
    /// `models.toml`. `threads` is ONNX Runtime's intra-op thread count.
    pub fn load(onnx: &Path, tokenizer: &Path, threads: usize) -> Result<Self, TtsError> {
        let vocab = read_vocab(tokenizer)?;
        let session = Session::builder()
            .map_err(engine("ONNX Runtime"))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| TtsError::Engine(format!("ONNX Runtime: {e}")))?
            .with_intra_threads(threads.max(1))
            .map_err(|e| TtsError::Engine(format!("ONNX Runtime: {e}")))?
            .commit_from_file(onnx)
            .map_err(engine("loading the Kokoro model"))?;
        Ok(Self { session, vocab })
    }

    /// Phoneme ids for `phonemes`; symbols outside Kokoro's vocabulary are
    /// dropped.
    pub fn tokenize(&self, phonemes: &str) -> Vec<i64> {
        phonemes
            .chars()
            .filter_map(|c| self.vocab.get(&c).copied())
            .collect()
    }

    /// Synthesize one utterance. Inputs longer than [`MAX_TOKENS`] are split
    /// at word boundaries and the pieces joined.
    pub fn synthesize(
        &mut self,
        phonemes: &str,
        voice: &VoiceStyle,
        speed: f32,
    ) -> Result<Vec<f32>, TtsError> {
        let mut samples = Vec::new();
        for piece in split_phonemes(phonemes, MAX_TOKENS, |s| self.tokenize(s).len()) {
            let ids = self.tokenize(&piece);
            if ids.is_empty() {
                continue;
            }
            samples.extend(self.run(&ids, voice, speed)?);
        }
        Ok(samples)
    }

    fn run(&mut self, ids: &[i64], voice: &VoiceStyle, speed: f32) -> Result<Vec<f32>, TtsError> {
        let mut input_ids = Vec::with_capacity(ids.len() + 2);
        input_ids.push(0);
        input_ids.extend_from_slice(ids);
        input_ids.push(0);
        let len = input_ids.len();
        let tensor = |e: ort::Error| TtsError::Engine(format!("Kokoro input: {e}"));
        let outputs = self
            .session
            .run(ort::inputs![
                "input_ids" => Tensor::from_array(([1usize, len], input_ids)).map_err(tensor)?,
                "style" => Tensor::from_array(([1usize, voice.row(ids.len()).len()], voice.row(ids.len()).to_vec())).map_err(tensor)?,
                "speed" => Tensor::from_array(([1usize], vec![speed])).map_err(tensor)?,
            ])
            .map_err(engine("Kokoro inference"))?;
        let (_, samples) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(engine("Kokoro output"))?;
        // A working model never emits NaN or infinity. The fp16 export does
        // on ONNX Runtime's CPU path (half-precision overflow), so it is
        // rejected here instead of played as clicks and gaps.
        let bad = samples.iter().filter(|s| !s.is_finite()).count();
        if bad > 0 {
            return Err(TtsError::Engine(format!(
                "Kokoro produced {bad} invalid (NaN or infinite) samples out of {}; \
                 this model file does not work here, use fp32",
                samples.len()
            )));
        }
        Ok(samples.to_vec())
    }
}

/// Kokoro's vocabulary from `tokenizer.json` (`model.vocab`: one phoneme
/// symbol to one id). Parsed as JSON only.
fn read_vocab(path: &Path) -> Result<HashMap<char, i64>, TtsError> {
    let fail = |why: String| TtsError::Engine(format!("{}: {why}", path.display()));
    let text = std::fs::read_to_string(path).map_err(|e| fail(e.to_string()))?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| fail(e.to_string()))?;
    let entries = json["model"]["vocab"]
        .as_object()
        .ok_or_else(|| fail("no model.vocab object".into()))?;
    let mut vocab = HashMap::with_capacity(entries.len());
    for (symbol, id) in entries {
        let mut chars = symbol.chars();
        if let (Some(c), None, Some(id)) = (chars.next(), chars.next(), id.as_i64()) {
            vocab.insert(c, id);
        }
    }
    if vocab.is_empty() {
        return Err(fail("the vocabulary is empty".into()));
    }
    Ok(vocab)
}

/// Split a phoneme string into pieces of at most `max` tokens, at spaces
/// where possible.
fn split_phonemes(phonemes: &str, max: usize, tokens: impl Fn(&str) -> usize) -> Vec<String> {
    if tokens(phonemes) <= max {
        return vec![phonemes.to_owned()];
    }
    let mut pieces = Vec::new();
    let mut current = String::new();
    for word in phonemes.split(' ') {
        let candidate = if current.is_empty() {
            word.to_owned()
        } else {
            format!("{current} {word}")
        };
        if tokens(&candidate) <= max {
            current = candidate;
            continue;
        }
        if !current.is_empty() {
            pieces.push(std::mem::take(&mut current));
        }
        // One word longer than the limit: cut it.
        let mut rest = word.to_owned();
        while tokens(&rest) > max {
            let cut: String = rest.chars().take(max).collect();
            rest = rest.chars().skip(max).collect();
            pieces.push(cut);
        }
        current = rest;
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_input_is_one_piece() {
        assert_eq!(
            split_phonemes("ab cd", 10, |s| s.chars().count()),
            ["ab cd"]
        );
    }

    #[test]
    fn long_input_splits_at_spaces_and_cuts_overlong_words() {
        let pieces = split_phonemes("aaa bbb ccc", 7, |s| s.chars().count());
        assert_eq!(pieces, ["aaa bbb", "ccc"]);
        let pieces = split_phonemes("abcdefghij", 4, |s| s.chars().count());
        assert_eq!(pieces, ["abcd", "efgh", "ij"]);
        for piece in split_phonemes(&"word ".repeat(400), 510, |s| s.chars().count()) {
            assert!(piece.chars().count() <= 510);
        }
    }
}
