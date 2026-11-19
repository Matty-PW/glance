//! `personality`: how answers sound, as a short style layer appended to the
//! end of the system prompt (FAST, escalation and DEEP alike). The prompts'
//! own instructions are never edited, and the layer says nothing about
//! length: `response_length` and the sentence cap in code decide that.
//!
//! `Neutral` adds nothing, so its prompts are byte-for-byte the ones
//! measured before personalities existed. Every other layer ends with the
//! same fixed rule (spoken sentences, no emoji, no markdown, no lists), after
//! any custom text, so a custom style cannot undo it.
//!
//! The fun presets (pirate, caveman, Shakespearean) play a character. On
//! the 4B a mild wording ("Talk like a pirate.") changed almost nothing, so
//! theirs name the character's words outright; they still say nothing about
//! length. Words the model can loop on are left out: with "aye" in the
//! pirate list and "as in: me see big number" in the caveman one, the FAST
//! eval had one answer each repeating "aye, aye, …" and "Me see …" to the
//! token limit.

/// Longest custom style, in characters.
pub const CUSTOM_MAX: usize = 300;

/// Closes every non-neutral layer.
pub const SPOKEN_RULE: &str =
    "Whatever the style, answer in plain spoken sentences: no emoji, no markdown, no lists.";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Personality {
    #[default]
    Neutral,
    Friendly,
    Direct,
    Strict,
    Pirate,
    Caveman,
    Shakespearean,
    /// Already cleaned by [`Personality::parse`].
    Custom(String),
}

/// The preset names, in the order the settings window lists them.
pub const PRESETS: [&str; 8] = [
    "neutral",
    "friendly",
    "direct",
    "strict",
    "pirate",
    "caveman",
    "shakespearean",
    "custom",
];

/// Closes the fun presets' style, before [`SPOKEN_RULE`].
const KEEP_FACTS: &str = "Keep names, numbers and errors exactly as the screen shows them.";

impl Personality {
    /// From the config's `personality` and `personality_custom`. Custom text
    /// is trimmed and put on one line; it must be 1 to 300 characters with
    /// no control characters.
    pub fn parse(preset: &str, custom: &str) -> Result<Self, String> {
        Ok(match preset {
            "neutral" => Self::Neutral,
            "friendly" => Self::Friendly,
            "direct" => Self::Direct,
            "strict" => Self::Strict,
            "pirate" => Self::Pirate,
            "caveman" => Self::Caveman,
            "shakespearean" => Self::Shakespearean,
            "custom" => Self::Custom(clean_custom(custom)?),
            other => {
                return Err(format!(
                    "personality must be neutral, friendly, direct, strict, pirate, caveman, \
                     shakespearean or custom, got \"{other}\""
                ));
            }
        })
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Neutral => "neutral",
            Self::Friendly => "friendly",
            Self::Direct => "direct",
            Self::Strict => "strict",
            Self::Pirate => "pirate",
            Self::Caveman => "caveman",
            Self::Shakespearean => "shakespearean",
            Self::Custom(_) => "custom",
        }
    }

    /// The style sentence(s) of a preset (custom: the user's text).
    fn style(&self) -> Option<String> {
        let fun = |character: &str| Some(format!("{character} {KEEP_FACTS}"));
        Some(match self {
            Self::Neutral => return None,
            // The first wordings ("Sound warm and friendly, like a helpful
            // colleague." and "Be strict and exact: say only what the screen
            // supports, and say plainly when something is wrong.") dropped
            // the exact type from an error (FAST eval 11/12 and 10/12), so
            // both now ask for on-screen details word for word.
            Self::Friendly => {
                "Sound warm and friendly, like a helpful colleague, and keep names, numbers \
                 and errors exactly as the screen shows them."
            }
            Self::Direct => "Be direct: start with the answer itself, with no pleasantries.",
            Self::Strict => {
                "Be strict and exact: give names, numbers and errors word for word as the \
                 screen shows them, and say plainly when something is wrong."
            }
            Self::Pirate => {
                return fun(
                    "Speak in character as a salty old pirate, with pirate words such as arr, \
                     matey, ye and me hearty.",
                );
            }
            Self::Caveman => {
                return fun(
                    "Speak in character as a caveman: broken grammar, no little words like \
                     the or is, and me instead of I.",
                );
            }
            Self::Shakespearean => {
                return fun(
                    "Speak in character as a player in a Shakespeare play, with thee, thou, \
                     thy, doth, hath and prithee.",
                );
            }
            Self::Custom(text) => text,
        }
        .to_owned())
    }

    /// What is appended to the system prompt; `None` for neutral.
    pub fn layer(&self) -> Option<String> {
        self.style()
            .map(|style| format!("\n\nStyle: {style} {SPOKEN_RULE}"))
    }
}

/// Trim, put on one line and bound custom style text (see [`Personality::parse`]).
pub fn clean_custom(text: &str) -> Result<String, String> {
    if text.chars().any(|c| c.is_control() && !c.is_whitespace()) {
        return Err("personality_custom must not contain control characters".into());
    }
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = one_line.chars().count();
    if count == 0 {
        return Err("personality = \"custom\" needs personality_custom text".into());
    }
    if count > CUSTOM_MAX {
        return Err(format!(
            "personality_custom is {count} characters; the limit is {CUSTOM_MAX}"
        ));
    }
    Ok(one_line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_adds_nothing() {
        assert_eq!(
            Personality::parse("neutral", "ignored").unwrap().layer(),
            None
        );
        assert_eq!(Personality::default(), Personality::Neutral);
    }

    #[test]
    fn every_other_preset_ends_with_the_spoken_rule() {
        for name in ["friendly", "direct", "strict", "pirate", "caveman", "shakespearean"] {
            let p = Personality::parse(name, "").unwrap();
            assert_eq!(p.name(), name);
            let layer = p.layer().unwrap();
            assert!(layer.starts_with("\n\nStyle: "), "{layer}");
            assert!(layer.ends_with(SPOKEN_RULE), "{layer}");
            // Length is the code's job, not the style's.
            for word in ["short", "brief", "concise", "detail", "sentence "] {
                assert!(
                    !layer[..layer.len() - SPOKEN_RULE.len()].contains(word),
                    "{name}: {word}"
                );
            }
        }
    }

    #[test]
    fn custom_text_is_cleaned_and_bounded() {
        let p = Personality::parse("custom", "  Calm,\n like a  pilot. ").unwrap();
        assert_eq!(p, Personality::Custom("Calm, like a pilot.".into()));
        assert_eq!(
            p.layer().unwrap(),
            format!("\n\nStyle: Calm, like a pilot. {SPOKEN_RULE}")
        );
        assert!(Personality::parse("custom", &"é".repeat(300)).is_ok());
        assert!(Personality::parse("custom", &"é".repeat(301)).is_err());
        assert!(Personality::parse("custom", "   ").is_err());
        assert!(Personality::parse("custom", "a\u{7}b").is_err());
        assert!(Personality::parse("robot", "").is_err());
    }
}
