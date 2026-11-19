//! `math_as_words`: say maths symbols as words ("2 × 2 = 4" is spoken
//! "2 times 2 equals 4"). A plain text rewrite of what goes to the voice,
//! applied after the model has answered: the answer itself, its accuracy and
//! the overlay text are unchanged.
//!
//! Conservative on purpose, so ordinary text is never mangled:
//! - `=`, `==`, `≠`, `≈`, `≤`, `≥`, `+`, `×`, `÷` and `^` are always maths;
//! - `x`, `*`, `-`, `/`, `<` and `>` count only between numbers (or single
//!   letters, as in "a - b"), standing on their own with spaces around;
//! - inside one token ("2x2=4", "10-4=6") only when it also has an `=` or
//!   one of the always-maths signs, so "Wi-Fi", "1920x1080", "11/19" and
//!   "a/b/c" stay as they are.

/// Rewrite `text` for speaking. Returns it unchanged when there is no maths.
pub fn math_as_words(text: &str) -> String {
    let tokens: Vec<&str> = text.split(' ').collect();
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    for (i, token) in tokens.iter().enumerate() {
        let prev = i.checked_sub(1).map(|j| tokens[j]);
        let next = tokens.get(i + 1).copied();
        out.push(match spaced_operator(token, prev, next) {
            Some(word) => word.to_owned(),
            None => inside_token(token).unwrap_or_else(|| (*token).to_owned()),
        });
    }
    out.join(" ")
}

/// Signs that are maths wherever they stand.
fn always(sign: &str) -> Option<&'static str> {
    Some(match sign {
        "=" | "==" => "equals",
        "≠" | "!=" => "does not equal",
        "≈" => "is about",
        "≤" | "<=" => "is less than or equal to",
        "≥" | ">=" => "is greater than or equal to",
        "+" => "plus",
        "×" | "·" => "times",
        "÷" => "divided by",
        "^" => "to the power of",
        "−" => "minus",
        _ => return None,
    })
}

/// Signs that are maths only between operands.
fn between(sign: &str) -> Option<&'static str> {
    Some(match sign {
        "x" | "X" | "*" => "times",
        "-" | "–" => "minus",
        "/" => "divided by",
        "<" => "is less than",
        ">" => "is greater than",
        _ => return None,
    })
}

/// A number ("2", "1,250", "3.5", "$40", "86%", "(3"), or one letter.
fn is_operand(token: &str) -> bool {
    let t = token
        .trim_start_matches(['(', '$', '£', '€'])
        .trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '%']);
    if t.chars().count() == 1 && t.chars().all(char::is_alphabetic) {
        return true;
    }
    !t.is_empty()
        && t.chars().next().is_some_and(|c| c.is_ascii_digit())
        && t.chars()
            .all(|c| c.is_ascii_digit() || c == ',' || c == '.')
}

fn spaced_operator(token: &str, prev: Option<&str>, next: Option<&str>) -> Option<&'static str> {
    if let Some(word) = always(token) {
        // "=" after nothing ("= 4") still reads as "equals 4".
        return Some(word);
    }
    let word = between(token)?;
    (prev.is_some_and(is_operand) && next.is_some_and(is_operand)).then_some(word)
}

/// "2x2=4", "3+4", "10-4=6": a token of numbers and signs only.
fn inside_token(token: &str) -> Option<String> {
    let core = token.trim_end_matches(['.', ',', ';', ':', '!', '?']);
    let tail = &token[core.len()..];
    let is_sign = |c: char| "=≠≈≤≥+×·÷^−x*-–/<>".contains(c);
    if !core.chars().any(|c| c.is_ascii_digit())
        || !core
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ',' || c == '!' || is_sign(c))
    {
        return None;
    }
    let strong = core.contains('=') || core.chars().any(|c| "≠≈≤≥+×·÷^−".contains(c));
    if !strong {
        return None;
    }
    // Split into numbers and signs, then name each sign.
    let mut parts: Vec<String> = Vec::new();
    let mut number = String::new();
    let mut sign = String::new();
    for c in core.chars() {
        if c.is_ascii_digit() || c == '.' || c == ',' {
            if !sign.is_empty() {
                parts.push(name_sign(&sign)?);
                sign.clear();
            }
            number.push(c);
        } else {
            if !number.is_empty() {
                parts.push(std::mem::take(&mut number));
            }
            sign.push(c);
        }
    }
    if !sign.is_empty() {
        parts.push(name_sign(&sign)?);
    }
    if !number.is_empty() {
        parts.push(number);
    }
    // A leading sign stays a word too ("−5" is "minus 5").
    Some(format!("{}{tail}", parts.join(" ")))
}

fn name_sign(sign: &str) -> Option<String> {
    always(sign).or_else(|| between(sign)).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::math_as_words as say;

    #[test]
    fn spaced_expressions() {
        assert_eq!(say("2 x 2 = 4"), "2 times 2 equals 4");
        assert_eq!(say("2 × 2 = 4."), "2 times 2 equals 4.");
        assert_eq!(
            say("So 10 - 4 = 6, then 6 / 2 = 3."),
            "So 10 minus 4 equals 6, then 6 divided by 2 equals 3."
        );
        assert_eq!(
            say("x = 5 and a + b ≠ c"),
            "x equals 5 and a plus b does not equal c"
        );
        assert_eq!(
            say("If 3 < 5 then 2^3 is 8"),
            "If 3 is less than 5 then 2 to the power of 3 is 8"
        );
        assert_eq!(say("left == right"), "left equals right");
        assert_eq!(
            say("$48,200 - $1,450 = $46,750"),
            "$48,200 minus $1,450 equals $46,750"
        );
    }

    #[test]
    fn expressions_without_spaces() {
        assert_eq!(say("2x2=4"), "2 times 2 equals 4");
        assert_eq!(say("10-4=6."), "10 minus 4 equals 6.");
        assert_eq!(say("3+4 is 7"), "3 plus 4 is 7");
        assert_eq!(say("2^10"), "2 to the power of 10");
    }

    #[test]
    fn ordinary_text_is_left_alone() {
        for text in [
            "You are on the Harbor-5G Wi-Fi network.",
            "The screen is 1920x1080.",
            "The meeting is on 11/19 at 2:30 pm.",
            "Open src/main.rs - it has the error.",
            "It costs $51,900, up 86%.",
            "The range is 5-10 items.",
            "a/b/c",
            "Use x to close the window.",
            "The temperature is -5 degrees.",
            "error[E0308]: mismatched types, expected u64",
        ] {
            assert_eq!(say(text), text);
        }
    }
}
