//! Kokoro voices: style tables loaded under the `raw-f32` rule
//! (`docs/DESIGN.md` §8). A voice file is 510 rows of 256 little-endian
//! `f32`s, read as plain floats after an exact length check. No
//! deserializer is involved.
//!
//! A blend is the weighted average of two or more tables. Kokoro takes one
//! 256-float style row per utterance, so averaging rows gives a voice in
//! between (the same trick the Kokoro authors use for their own blends).

use std::fmt;
use std::path::Path;

use crate::TtsError;

/// Rows in a style table: one per possible input length in tokens.
pub const ROWS: usize = 510;
/// Floats per style row.
pub const WIDTH: usize = 256;
/// The exact size of a voice file in bytes.
pub const FILE_BYTES: usize = ROWS * WIDTH * 4;

/// One voice (or blend) ready for synthesis.
#[derive(Clone)]
pub struct VoiceStyle {
    table: Vec<f32>,
}

impl fmt::Debug for VoiceStyle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VoiceStyle({ROWS}x{WIDTH})")
    }
}

impl VoiceStyle {
    /// Read a voice file. The caller has already checked its SHA-256
    /// against `models.toml`.
    pub fn load(path: &Path) -> Result<Self, TtsError> {
        let bytes = std::fs::read(path)
            .map_err(|e| TtsError::Engine(format!("{}: {e}", path.display())))?;
        Self::from_bytes(&bytes).map_err(|e| TtsError::Engine(format!("{}: {e}", path.display())))
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() != FILE_BYTES {
            return Err(format!(
                "a voice file must be exactly {FILE_BYTES} bytes ({ROWS}x{WIDTH} f32), got {}",
                bytes.len()
            ));
        }
        let table: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        if table.iter().any(|x| !x.is_finite()) {
            return Err("a voice file contains NaN or infinite values".into());
        }
        Ok(Self { table })
    }

    /// Weighted average of voices. Weights must be positive; they are
    /// normalized, so `[(a, 3.0), (b, 1.0)]` is 75 % `a`.
    pub fn blend(parts: &[(&VoiceStyle, f32)]) -> Result<Self, String> {
        if parts.is_empty() {
            return Err("a blend needs at least one voice".into());
        }
        if parts.iter().any(|(_, w)| !w.is_finite() || *w <= 0.0) {
            return Err("blend weights must be positive numbers".into());
        }
        let total: f32 = parts.iter().map(|(_, w)| w).sum();
        let mut table = vec![0.0f32; ROWS * WIDTH];
        for (voice, weight) in parts {
            let share = weight / total;
            for (out, x) in table.iter_mut().zip(&voice.table) {
                *out += x * share;
            }
        }
        Ok(Self { table })
    }

    /// The style row for an input of `tokens` phoneme tokens (Kokoro's own
    /// pipeline uses row `len - 1`).
    pub fn row(&self, tokens: usize) -> &[f32] {
        let r = tokens.saturating_sub(1).min(ROWS - 1);
        &self.table[r * WIDTH..(r + 1) * WIDTH]
    }
}

/// Which voice to use: one id, or a weighted blend of several.
#[derive(Debug, Clone, PartialEq)]
pub enum VoiceChoice {
    Single(String),
    Blend(Vec<(String, f32)>),
}

impl VoiceChoice {
    /// `af_heart`, or a blend written `af_heart:0.6,bf_emma:0.4`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if !text.contains([',', ':']) {
            check_id(text)?;
            return Ok(VoiceChoice::Single(text.to_owned()));
        }
        let mut parts = Vec::new();
        for item in text.split(',') {
            let (id, weight) = item
                .split_once(':')
                .ok_or_else(|| format!("`{item}`: a blend is written id:weight,id:weight"))?;
            let id = id.trim();
            check_id(id)?;
            let weight: f32 = weight
                .trim()
                .parse()
                .map_err(|_| format!("`{item}`: the weight is not a number"))?;
            parts.push((id.to_owned(), weight));
        }
        Ok(VoiceChoice::Blend(parts))
    }

    /// The voice ids involved.
    pub fn ids(&self) -> Vec<&str> {
        match self {
            VoiceChoice::Single(id) => vec![id.as_str()],
            VoiceChoice::Blend(parts) => parts.iter().map(|(id, _)| id.as_str()).collect(),
        }
    }

    /// The voice whose accent the phonemizer follows: the single voice, or
    /// the most heavily weighted one in a blend.
    pub fn lead(&self) -> &str {
        match self {
            VoiceChoice::Single(id) => id,
            VoiceChoice::Blend(parts) => parts
                .iter()
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .map_or("", |(id, _)| id.as_str()),
        }
    }
}

impl fmt::Display for VoiceChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VoiceChoice::Single(id) => f.write_str(id),
            VoiceChoice::Blend(parts) => {
                let items: Vec<String> = parts.iter().map(|(id, w)| format!("{id}:{w}")).collect();
                f.write_str(&items.join(","))
            }
        }
    }
}

/// Voice ids are short lowercase names like `af_heart`: they become file
/// names, so nothing else is accepted.
fn check_id(id: &str) -> Result<(), String> {
    let ok =
        !id.is_empty() && id.len() <= 32 && id.chars().all(|c| c.is_ascii_lowercase() || c == '_');
    if ok {
        Ok(())
    } else {
        Err(format!("`{id}` is not a voice id (like af_heart)"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voice(value: f32) -> VoiceStyle {
        let bytes: Vec<u8> = std::iter::repeat_n(value.to_le_bytes(), ROWS * WIDTH)
            .flatten()
            .collect();
        VoiceStyle::from_bytes(&bytes).unwrap()
    }

    #[test]
    fn length_is_checked_exactly() {
        assert!(VoiceStyle::from_bytes(&vec![0; FILE_BYTES - 4]).is_err());
        assert!(VoiceStyle::from_bytes(&vec![0; FILE_BYTES + 4 * WIDTH]).is_err());
        assert!(VoiceStyle::from_bytes(&vec![0; FILE_BYTES]).is_ok());
    }

    #[test]
    fn non_finite_values_are_rejected() {
        let mut bytes = vec![0u8; FILE_BYTES];
        bytes[..4].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(VoiceStyle::from_bytes(&bytes).is_err());
    }

    #[test]
    fn rows_are_picked_by_token_count_and_clamped() {
        let mut bytes = vec![0u8; FILE_BYTES];
        let row = 4;
        bytes[row * WIDTH * 4..row * WIDTH * 4 + 4].copy_from_slice(&7.0f32.to_le_bytes());
        let v = VoiceStyle::from_bytes(&bytes).unwrap();
        assert_eq!(v.row(5)[0], 7.0);
        assert_eq!(v.row(5).len(), WIDTH);
        assert_eq!(v.row(10_000).len(), WIDTH);
        assert_eq!(v.row(0).len(), WIDTH);
    }

    #[test]
    fn a_blend_is_the_normalized_weighted_mean() {
        let blended = VoiceStyle::blend(&[(&voice(1.0), 3.0), (&voice(5.0), 1.0)]).unwrap();
        assert!((blended.row(1)[0] - 2.0).abs() < 1e-6);
        assert!(VoiceStyle::blend(&[(&voice(1.0), 0.0)]).is_err());
        assert!(VoiceStyle::blend(&[]).is_err());
    }

    #[test]
    fn voice_choices_parse() {
        assert_eq!(
            VoiceChoice::parse("af_heart").unwrap(),
            VoiceChoice::Single("af_heart".into())
        );
        let blend = VoiceChoice::parse("af_heart:0.6, bf_emma:0.4").unwrap();
        assert_eq!(
            blend,
            VoiceChoice::Blend(vec![("af_heart".into(), 0.6), ("bf_emma".into(), 0.4)])
        );
        assert_eq!(blend.lead(), "af_heart");
        assert_eq!(blend.to_string(), "af_heart:0.6,bf_emma:0.4");
        assert!(VoiceChoice::parse("../etc/passwd").is_err());
        assert!(VoiceChoice::parse("af_heart:x").is_err());
    }
}
