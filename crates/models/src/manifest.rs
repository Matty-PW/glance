//! `models.toml` parsing and validation. Every rule in `docs/DESIGN.md` §8 that
//! can be checked without touching the network or the disk is enforced here.

use std::collections::HashSet;

use serde::Deserialize;

/// The manifest compiled into the binary.
pub const BUILTIN: &str = include_str!("../../../models.toml");

/// File extensions that are never accepted, whatever the manifest says.
const FORBIDDEN_EXTENSIONS: &[&str] =
    &["pt", "pth", "pkl", "pickle", "ckpt", "npz", "npy", "joblib"];

/// A Kokoro style vector is 256 little-endian `f32`s.
const RAW_F32_ROW_BYTES: u64 = 256 * 4;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ManifestError {
    #[error("manifest is not valid TOML for this schema: {0}")]
    Parse(String),
    #[error("unsupported manifest schema {0} (expected 1)")]
    Schema(u32),
    #[error("model `{model}`: {reason}")]
    Model { model: String, reason: String },
    #[error("model `{model}`, file `{file}`: {reason}")]
    File {
        model: String,
        file: String,
        reason: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    schema: u32,
    #[serde(default, rename = "model")]
    models: Vec<Model>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub id: String,
    pub role: Role,
    pub license: String,
    pub repo: String,
    pub revision: String,
    #[serde(default, rename = "file")]
    pub files: Vec<ModelFile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelFile {
    pub path: String,
    pub kind: Kind,
    pub format: Format,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Vlm,
    Stt,
    Tts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Weights,
    Mmproj,
    Voice,
    Aux,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Format {
    #[serde(rename = "gguf")]
    Gguf,
    #[serde(rename = "onnx")]
    Onnx,
    #[serde(rename = "safetensors")]
    Safetensors,
    #[serde(rename = "raw-f32")]
    RawF32,
    #[serde(rename = "text")]
    Text,
}

impl Manifest {
    /// Parse and validate. A manifest that fails any rule is rejected whole.
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let manifest: Manifest =
            toml::from_str(text).map_err(|e| ManifestError::Parse(e.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// The manifest compiled into the binary.
    pub fn builtin() -> Result<Self, ManifestError> {
        Self::parse(BUILTIN)
    }

    pub fn models(&self) -> &[Model] {
        &self.models
    }

    pub fn model(&self, id: &str) -> Option<&Model> {
        self.models.iter().find(|m| m.id == id)
    }

    fn validate(&self) -> Result<(), ManifestError> {
        if self.schema != 1 {
            return Err(ManifestError::Schema(self.schema));
        }
        let mut seen = HashSet::new();
        for model in &self.models {
            model.validate()?;
            if !seen.insert(model.id.as_str()) {
                return Err(model.err("duplicate id"));
            }
        }
        Ok(())
    }
}

impl Model {
    fn err(&self, reason: &str) -> ManifestError {
        ManifestError::Model {
            model: self.id.clone(),
            reason: reason.to_owned(),
        }
    }

    fn validate(&self) -> Result<(), ManifestError> {
        if self.id.is_empty()
            || !self
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
            || self.id.starts_with('.')
        {
            return Err(self.err("id must be lowercase letters, digits, `.`, `_` or `-`"));
        }
        if self.license.trim().is_empty() {
            return Err(self.err("license is required"));
        }
        if !is_repo(&self.repo) {
            return Err(self.err("repo must look like `owner/name`"));
        }
        if !is_lower_hex(&self.revision, 40) {
            return Err(
                self.err("revision must be a full 40-character commit hash, not a branch or tag")
            );
        }
        if self.files.is_empty() {
            return Err(self.err("model has no files"));
        }
        let mut seen = HashSet::new();
        for file in &self.files {
            file.validate().map_err(|reason| ManifestError::File {
                model: self.id.clone(),
                file: file.path.clone(),
                reason,
            })?;
            if !seen.insert(file.path.as_str()) {
                return Err(self.err("duplicate file path"));
            }
        }
        Ok(())
    }

    /// The only URL shape glance ever requests for a model file.
    pub fn url(&self, file: &ModelFile) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            self.repo, self.revision, file.path
        )
    }
}

impl ModelFile {
    fn validate(&self) -> Result<(), String> {
        let path = &self.path;
        let safe_chars = path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-/".contains(c));
        if path.is_empty()
            || !safe_chars
            || path.starts_with('/')
            || path.ends_with('/')
            || path
                .split('/')
                .any(|part| part.is_empty() || part.starts_with('.'))
        {
            return Err("path must be a plain relative path".to_owned());
        }

        let lower = path.to_ascii_lowercase();
        let ext = lower.rsplit('.').next().unwrap_or("");
        if FORBIDDEN_EXTENSIONS.contains(&ext) {
            return Err(format!("`.{ext}` files are never loaded"));
        }

        let (allowed_kinds, allowed_exts): (&[Kind], &[&str]) = match self.format {
            Format::Gguf => (&[Kind::Weights, Kind::Mmproj], &["gguf"]),
            Format::Onnx => (&[Kind::Weights], &["onnx"]),
            Format::Safetensors => (&[Kind::Weights], &["safetensors"]),
            Format::RawF32 => (&[Kind::Voice], &["bin"]),
            Format::Text => (&[Kind::Aux], &["txt", "json"]),
        };
        if !allowed_kinds.contains(&self.kind) {
            return Err(format!(
                "format {:?} is not allowed for kind {:?}",
                self.format, self.kind
            ));
        }
        if !allowed_exts.contains(&ext) {
            return Err(format!(
                "format {:?} requires one of these extensions: {allowed_exts:?}",
                self.format
            ));
        }

        if self.size == 0 {
            return Err("size must be the exact byte count".to_owned());
        }
        if self.format == Format::RawF32 && !self.size.is_multiple_of(RAW_F32_ROW_BYTES) {
            return Err("raw-f32 size must be a multiple of 1024 bytes (256 floats)".to_owned());
        }
        if !is_lower_hex(&self.sha256, 64) {
            return Err("sha256 must be 64 lowercase hex characters".to_owned());
        }
        Ok(())
    }
}

fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_repo(s: &str) -> bool {
    let mut parts = s.split('/');
    let ok = |p: Option<&str>| {
        p.is_some_and(|p| {
            !p.is_empty()
                && !p.starts_with('.')
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        })
    };
    ok(parts.next()) && ok(parts.next()) && parts.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    const REV: &str = "0123456789abcdef0123456789abcdef01234567";
    const SHA: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn manifest(file_lines: &str) -> String {
        format!(
            "schema = 1\n[[model]]\nid = \"m\"\nrole = \"vlm\"\nlicense = \"Apache-2.0\"\n\
             repo = \"org/repo\"\nrevision = \"{REV}\"\n[[model.file]]\n{file_lines}\n"
        )
    }

    fn file(path: &str, kind: &str, format: &str, size: u64) -> String {
        manifest(&format!(
            "path = \"{path}\"\nkind = \"{kind}\"\nformat = \"{format}\"\nsize = {size}\nsha256 = \"{SHA}\""
        ))
    }

    fn file_reason(text: &str) -> String {
        match Manifest::parse(text) {
            Err(ManifestError::File { reason, .. }) => reason,
            other => panic!("expected a file error, got {other:?}"),
        }
    }

    #[test]
    fn no_model_id_can_be_the_partial_directory() {
        let text = file("a.gguf", "weights", "gguf", 10).replace("id = \"m\"", "id = \".partial\"");
        assert!(matches!(
            Manifest::parse(&text),
            Err(ManifestError::Model { .. })
        ));
    }

    #[test]
    fn builtin_manifest_is_valid() {
        Manifest::builtin().expect("models.toml in the repo must always validate");
    }

    #[test]
    fn accepts_the_allowed_formats() {
        for (path, kind, format, size) in [
            ("a.gguf", "weights", "gguf", 10),
            ("mmproj-a.gguf", "mmproj", "gguf", 10),
            ("enc.int8.onnx", "weights", "onnx", 10),
            ("m.safetensors", "weights", "safetensors", 10),
            ("voices/af_heart.bin", "voice", "raw-f32", 522_240),
            ("vocab.txt", "aux", "text", 10),
            ("tokenizer.json", "aux", "text", 10),
        ] {
            let m = Manifest::parse(&file(path, kind, format, size))
                .unwrap_or_else(|e| panic!("{path}: {e}"));
            assert_eq!(m.models().len(), 1);
        }
    }

    #[test]
    fn url_is_pinned_to_huggingface_and_the_commit() {
        let m = Manifest::parse(&file("sub/a.gguf", "weights", "gguf", 10)).unwrap();
        let model = m.model("m").unwrap();
        assert_eq!(
            model.url(&model.files[0]),
            format!("https://huggingface.co/org/repo/resolve/{REV}/sub/a.gguf")
        );
    }

    #[test]
    fn rejects_pickle_formats_whatever_they_claim_to_be() {
        for path in ["a.pt", "a.pth", "a.pkl", "a.ckpt", "a.npz", "a.npy", "A.PT"] {
            for format in ["gguf", "onnx", "safetensors", "raw-f32", "text"] {
                let reason = file_reason(&file(path, "weights", format, 1024));
                assert!(reason.contains("never loaded"), "{path} {format}: {reason}");
            }
        }
    }

    #[test]
    fn bin_is_only_allowed_as_a_raw_f32_voice() {
        assert!(Manifest::parse(&file("a.bin", "voice", "raw-f32", 1024)).is_ok());
        // .bin as weights, in any weights format, is refused.
        for format in ["gguf", "onnx", "safetensors"] {
            assert!(Manifest::parse(&file("a.bin", "weights", format, 1024)).is_err());
        }
        // raw-f32 cannot be used for weights, and must be whole rows.
        assert!(Manifest::parse(&file("a.bin", "weights", "raw-f32", 1024)).is_err());
        assert!(Manifest::parse(&file("a.bin", "voice", "raw-f32", 1000)).is_err());
        // a voice cannot be smuggled in under another extension.
        assert!(Manifest::parse(&file("a.gguf", "voice", "raw-f32", 1024)).is_err());
    }

    #[test]
    fn text_is_only_for_aux_files() {
        assert!(Manifest::parse(&file("vocab.txt", "weights", "text", 10)).is_err());
        assert!(Manifest::parse(&file("model.gguf", "aux", "text", 10)).is_err());
    }

    #[test]
    fn rejects_unsafe_paths() {
        for path in [
            "/abs.gguf",
            "../up.gguf",
            "a/../b.gguf",
            "a//b.gguf",
            ".hidden.gguf",
            "a b.gguf",
            "a\\b.gguf",
            "dir/",
            // Unfinished downloads can never be named as a model file.
            ".partial/0123.part",
            "a/.partial/b.gguf",
            "a.gguf.part",
            "a.part",
        ] {
            assert!(
                Manifest::parse(&file(path, "weights", "gguf", 10)).is_err(),
                "{path} should be rejected"
            );
        }
    }

    #[test]
    fn requires_size_hash_license_and_a_commit() {
        assert!(Manifest::parse(&file("a.gguf", "weights", "gguf", 0)).is_err());
        let ok = file("a.gguf", "weights", "gguf", 10);
        assert!(Manifest::parse(&ok.replace(SHA, "abc")).is_err());
        assert!(Manifest::parse(&ok.replace(SHA, &SHA.to_uppercase())).is_err());
        assert!(Manifest::parse(&ok.replace("Apache-2.0", " ")).is_err());
        assert!(Manifest::parse(&ok.replace(REV, "main")).is_err());
        assert!(Manifest::parse(&ok.replace("org/repo", "https://evil.example/x")).is_err());
        assert!(Manifest::parse(&ok.replace("org/repo", "org/repo/extra")).is_err());
    }

    #[test]
    fn rejects_unknown_fields_schema_and_duplicates() {
        let ok = file("a.gguf", "weights", "gguf", 10);
        assert!(Manifest::parse(&ok.replace("size = 10", "size = 10\nurl = \"x\"")).is_err());
        assert_eq!(
            Manifest::parse(&ok.replace("schema = 1", "schema = 2")).unwrap_err(),
            ManifestError::Schema(2)
        );
        let second = ok.split_once("[[model]]").unwrap().1;
        let doubled = format!("{ok}[[model]]{second}");
        assert!(Manifest::parse(&doubled).is_err());
    }
}
