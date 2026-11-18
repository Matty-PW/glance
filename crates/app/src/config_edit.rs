//! Changing single settings in `config.toml` from the settings window while
//! keeping the user's file as written: every other line, comments included,
//! stays as it is. Only the edited `key = value` line is rewritten (a
//! comment at the end of that line is dropped). The result is parsed with
//! the app's strict parser before it replaces the file; if that fails, the
//! file is left untouched.
//!
//! The config uses flat `[section]` tables with string, number and boolean
//! values only, which is all this handles.

use std::path::Path;

use anyhow::{Context, Result, bail};

/// A value to write.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl Value {
    fn render(&self) -> String {
        match self {
            Value::Str(s) => {
                let escaped: String = s
                    .chars()
                    .flat_map(|c| match c {
                        '"' => vec!['\\', '"'],
                        '\\' => vec!['\\', '\\'],
                        '\n' => vec!['\\', 'n'],
                        c => vec![c],
                    })
                    .collect();
                format!("\"{escaped}\"")
            }
            Value::Int(n) => n.to_string(),
            // Keep one decimal so TOML reads it back as a float.
            Value::Float(f) => {
                let text = format!("{f}");
                if text.contains(['.', 'e', 'E']) {
                    text
                } else {
                    format!("{text}.0")
                }
            }
            Value::Bool(b) => b.to_string(),
        }
    }
}

/// Set (or with `None`, remove) `key` in `[section]` of `text`.
pub fn set_in(text: &str, section: &str, key: &str, value: Option<&Value>) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let header = format!("[{section}]");
    let start = lines.iter().position(|l| l.trim() == header);
    let Some(start) = start else {
        if let Some(value) = value {
            if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push(header);
            lines.push(format!("{key} = {}", value.render()));
        }
        return join(lines);
    };
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .map_or(lines.len(), |i| start + 1 + i);
    let is_key = |line: &str| {
        let t = line.trim_start();
        t.strip_prefix(key)
            .is_some_and(|rest| rest.trim_start().starts_with('='))
    };
    match (lines[start + 1..end].iter().position(|l| is_key(l)), value) {
        (Some(i), Some(value)) => lines[start + 1 + i] = format!("{key} = {}", value.render()),
        (Some(i), None) => {
            lines.remove(start + 1 + i);
        }
        (None, Some(value)) => {
            // After the section's last non-blank line.
            let last = (start..end)
                .rev()
                .find(|&i| !lines[i].trim().is_empty())
                .unwrap_or(start);
            lines.insert(last + 1, format!("{key} = {}", value.render()));
        }
        (None, None) => {}
    }
    join(lines)
}

fn join(lines: Vec<String>) -> String {
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// Apply several edits to the file at `path` (created if missing), check
/// the result with `validate`, then replace the file.
pub fn update(
    path: &Path,
    edits: &[(&str, &str, Option<Value>)],
    validate: impl Fn(&str) -> Result<()>,
) -> Result<()> {
    let original = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut text = original.clone();
    for (section, key, value) in edits {
        text = set_in(&text, section, key, value.as_ref());
    }
    if text == original {
        return Ok(());
    }
    if let Err(e) = validate(&text) {
        bail!("the change was not saved, it would make config.toml invalid: {e:#}");
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension("toml.new");
    std::fs::write(&temp, &text).with_context(|| format!("writing {}", temp.display()))?;
    std::fs::rename(&temp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "# my glance settings\n\n[speech]\n# a nicer voice\nvoice = \"af_heart\"\nspeed = 1.0  # default\n\n[models]\nfast = \"qwen3-vl-4b-instruct-q4km\"\n";

    #[test]
    fn replacing_a_value_keeps_every_other_line() {
        let out = set_in(FILE, "speech", "voice", Some(&Value::Str("bf_emma".into())));
        assert_eq!(
            out,
            FILE.replace("voice = \"af_heart\"", "voice = \"bf_emma\"")
        );
    }

    #[test]
    fn a_missing_key_goes_at_the_end_of_its_section() {
        let out = set_in(FILE, "speech", "volume", Some(&Value::Float(0.5)));
        assert!(
            out.contains("speed = 1.0  # default\nvolume = 0.5\n\n[models]"),
            "{out}"
        );
    }

    #[test]
    fn a_missing_section_is_appended_and_removal_works() {
        let out = set_in(
            FILE,
            "hotkeys",
            "think",
            Some(&Value::Str("Ctrl+Opt+Y".into())),
        );
        assert!(
            out.ends_with("\n[hotkeys]\nthink = \"Ctrl+Opt+Y\"\n"),
            "{out}"
        );
        let out = set_in(FILE, "models", "fast", None);
        assert!(!out.contains("fast ="));
        assert!(out.contains("[models]"));
        // Removing what is not there changes nothing.
        assert_eq!(set_in(FILE, "models", "deep", None), FILE);
    }

    #[test]
    fn keys_that_share_a_prefix_are_not_confused() {
        let text = "[models]\ndeep_effort = \"low\"\ndeep = \"x\"\n";
        let out = set_in(text, "models", "deep", Some(&Value::Str("y".into())));
        assert_eq!(out, "[models]\ndeep_effort = \"low\"\ndeep = \"y\"\n");
    }

    #[test]
    fn values_render_as_valid_toml() {
        assert_eq!(
            Value::Str("a \"b\" \\ c".into()).render(),
            r#""a \"b\" \\ c""#
        );
        assert_eq!(Value::Float(1.0).render(), "1.0");
        assert_eq!(Value::Float(1.25).render(), "1.25");
        assert_eq!(Value::Int(600).render(), "600");
        assert_eq!(Value::Bool(true).render(), "true");
    }

    #[test]
    fn an_invalid_result_leaves_the_file_alone() {
        let dir = std::env::temp_dir().join(format!("glance-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, FILE).unwrap();
        let err = update(
            &path,
            &[("speech", "speed", Some(Value::Float(9.0)))],
            |text| crate::config::Config::parse(text).map(|_| ()),
        )
        .unwrap_err();
        assert!(err.to_string().contains("not saved"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), FILE);
        update(
            &path,
            &[("speech", "speed", Some(Value::Float(1.2)))],
            |text| crate::config::Config::parse(text).map(|_| ()),
        )
        .unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("speed = 1.2\n")
        );
    }
}
