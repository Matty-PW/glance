//! Cleans model output so a speech engine reads it naturally: markdown
//! markers go, URLs shrink to their host, identifiers lose their underscores.

/// Returns `text` prepared for speech, or an empty string when nothing in it
/// can be spoken.
pub fn speakable(text: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = strip_line_prefix(line);
        for word in line.split_whitespace() {
            let word = clean_word(word);
            if !word.is_empty() {
                words.push(word);
            }
        }
    }
    let joined = words.join(" ");
    if joined.chars().any(char::is_alphanumeric) {
        joined
    } else {
        String::new()
    }
}

/// Drop heading hashes, list bullets and list numbers at the start of a line.
fn strip_line_prefix(line: &str) -> &str {
    let line = line.trim_start();
    let line = line.trim_start_matches('#').trim_start();
    if let Some(rest) = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| line.strip_prefix("• "))
    {
        return rest;
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if (1..=2).contains(&digits)
        && let Some(rest) = line[digits..]
            .strip_prefix(". ")
            .or_else(|| line[digits..].strip_prefix(") "))
    {
        return rest;
    }
    line
}

fn clean_word(word: &str) -> String {
    if let Some(host) = url_host(word) {
        return host;
    }
    let mut out = String::with_capacity(word.len());
    for c in word.chars() {
        match c {
            '*' | '`' | '~' => {}
            '_' => out.push(' '),
            _ => out.push(c),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `https://example.com/a/b?c` becomes `example.com`, keeping any trailing
/// sentence punctuation.
fn url_host(word: &str) -> Option<String> {
    let start = word.find("https://").or_else(|| word.find("http://"))?;
    let after_scheme = &word[start..];
    let rest = after_scheme
        .strip_prefix("https://")
        .or_else(|| after_scheme.strip_prefix("http://"))?;
    let host_end = rest
        .find(['/', '?', '#', ')', ']', '"', '>'])
        .unwrap_or(rest.len());
    let host = rest[..host_end].trim_end_matches(['.', ',', ';', ':', '!']);
    if host.is_empty() {
        return None;
    }
    let trailing: String = word
        .chars()
        .rev()
        .take_while(|c| matches!(c, '.' | ',' | ';' | ':' | '!' | '?'))
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    Some(format!("{}{host}{trailing}", &word[..start]))
}

#[cfg(test)]
mod tests {
    use super::speakable;

    #[test]
    fn strips_markdown() {
        assert_eq!(
            speakable("**Bold** and `code` and ~~gone~~."),
            "Bold and code and gone."
        );
        assert_eq!(speakable("## Heading"), "Heading");
        assert_eq!(speakable("- first item"), "first item");
        assert_eq!(speakable("2. second item"), "second item");
    }

    #[test]
    fn identifiers_are_read_as_words() {
        assert_eq!(
            speakable("The test refuses_wrong_hash failed."),
            "The test refuses wrong hash failed."
        );
    }

    #[test]
    fn urls_shrink_to_their_host() {
        assert_eq!(
            speakable("See https://docs.rs/cpal/0.18.2/cpal/ for details."),
            "See docs.rs for details."
        );
        assert_eq!(
            speakable("It is at http://example.com."),
            "It is at example.com."
        );
        assert_eq!(speakable("(https://example.com/a?b=c)"), "(example.com");
    }

    #[test]
    fn keeps_ordinary_text_and_numbers_untouched() {
        let text = "Marketing spent $51,900 in August, 3.5% over budget.";
        assert_eq!(speakable(text), text);
    }

    #[test]
    fn nothing_speakable_gives_an_empty_string() {
        for text in ["", "   ", "***", "---", "...", "`", "\n\n"] {
            assert_eq!(speakable(text), "", "{text:?}");
        }
    }

    #[test]
    fn collapses_whitespace_and_line_breaks() {
        assert_eq!(speakable("  one\n\n  two   three "), "one two three");
    }
}
