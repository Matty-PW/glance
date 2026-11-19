//! The on-disk model store and the "verify before use" gate.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::manifest::{Format, Model, ModelFile};

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("{path}: not downloaded")]
    Missing { path: PathBuf },
    #[error("{path}: size is {actual} bytes, manifest pins {expected}")]
    Size {
        path: PathBuf,
        expected: u64,
        actual: u64,
    },
    #[error("{path}: SHA-256 is {actual}, manifest pins {expected}")]
    Hash {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    #[error("{path}: does not look like a {format:?} file")]
    Magic { path: PathBuf, format: Format },
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot locate the model store: HOME is not set")]
    NoHome,
}

/// Root directory holding one sub-directory per model id.
#[derive(Debug, Clone)]
pub struct ModelStore {
    root: PathBuf,
}

impl ModelStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `~/Library/Application Support/glance/models`.
    pub fn default_location() -> Result<Self, VerifyError> {
        let home = std::env::var_os("HOME").ok_or(VerifyError::NoHome)?;
        Ok(Self::new(
            PathBuf::from(home).join("Library/Application Support/glance/models"),
        ))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn model_dir(&self, model: &Model) -> PathBuf {
        self.root.join(&model.id)
    }

    pub fn file_path(&self, model: &Model, file: &ModelFile) -> PathBuf {
        self.model_dir(model).join(&file.path)
    }

    pub(crate) fn partial_dir(&self) -> PathBuf {
        self.root.join(".partial")
    }

    /// Check one file against its pinned size, SHA-256 and format.
    pub fn verify_file(&self, model: &Model, file: &ModelFile) -> Result<PathBuf, VerifyError> {
        let path = self.file_path(model, file);
        check_file(&path, file)?;
        Ok(path)
    }

    /// How many of the model's files exist (presence and size only; no
    /// hashing, so cheap enough for a list).
    pub fn files_present(&self, model: &Model) -> usize {
        model
            .files
            .iter()
            .filter(|f| {
                std::fs::metadata(self.file_path(model, f))
                    .is_ok_and(|m| m.is_file() && m.len() == f.size)
            })
            .count()
    }

    /// Delete a model's files: only the paths its manifest entry lists (the
    /// manifest refuses `..` and absolute paths), then any directories left
    /// empty, then its unfinished downloads. Anything else in the model
    /// directory stays. Returns the bytes freed.
    pub fn remove(&self, model: &Model) -> std::io::Result<u64> {
        let mut freed = 0;
        let dir = self.model_dir(model);
        for file in &model.files {
            let path = self.file_path(model, file);
            match std::fs::symlink_metadata(&path) {
                Ok(meta) if meta.is_file() => {
                    std::fs::remove_file(&path)?;
                    freed += meta.len();
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            // Empty parents up to and including the model directory.
            let mut parent = path.parent();
            while let Some(p) = parent {
                if !p.starts_with(&dir) || std::fs::remove_dir(p).is_err() {
                    break;
                }
                parent = p.parent();
            }
        }
        freed += self.remove_partials(model);
        Ok(freed)
    }

    /// Delete unfinished downloads of this model's files (after a cancel).
    pub fn remove_partials(&self, model: &Model) -> u64 {
        let mut freed = 0;
        for file in &model.files {
            let partial = self.partial_dir().join(format!("{}.part", file.sha256));
            if let Ok(meta) = std::fs::metadata(&partial)
                && std::fs::remove_file(&partial).is_ok()
            {
                freed += meta.len();
            }
        }
        freed
    }

    /// Check every file of a model. Returns the model directory, which is the
    /// only way callers obtain a path to load from.
    pub fn verified_dir(&self, model: &Model) -> Result<PathBuf, VerifyError> {
        for file in &model.files {
            self.verify_file(model, file)?;
        }
        Ok(self.model_dir(model))
    }
}

/// Verify `path` against the manifest entry `file`.
pub(crate) fn check_file(path: &Path, file: &ModelFile) -> Result<(), VerifyError> {
    let io = |source| VerifyError::Io {
        path: path.to_owned(),
        source,
    };
    let mut handle = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(VerifyError::Missing {
                path: path.to_owned(),
            });
        }
        Err(e) => return Err(io(e)),
    };
    let actual_size = handle.metadata().map_err(io)?.len();
    if actual_size != file.size {
        return Err(VerifyError::Size {
            path: path.to_owned(),
            expected: file.size,
            actual: actual_size,
        });
    }

    let mut hasher = Sha256::new();
    let mut head = [0u8; 4];
    let mut head_len = 0;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = handle.read(&mut buf).map_err(io)?;
        if n == 0 {
            break;
        }
        if head_len < head.len() {
            let take = (head.len() - head_len).min(n);
            head[head_len..head_len + take].copy_from_slice(&buf[..take]);
            head_len += take;
        }
        hasher.update(&buf[..n]);
    }
    let actual = hex(&hasher.finalize());
    if actual != file.sha256 {
        return Err(VerifyError::Hash {
            path: path.to_owned(),
            expected: file.sha256.clone(),
            actual,
        });
    }
    if !magic_ok(file.format, &head[..head_len]) {
        return Err(VerifyError::Magic {
            path: path.to_owned(),
            format: file.format,
        });
    }
    Ok(())
}

/// Cheap format sanity check on the first bytes. Only GGUF has a magic number;
/// the other formats are covered by the extension rule and the hash.
pub(crate) fn magic_ok(format: Format, head: &[u8]) -> bool {
    match format {
        Format::Gguf => head == b"GGUF",
        Format::Onnx | Format::Safetensors | Format::RawF32 | Format::Text => true,
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;

    const ABC_SHA: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn temp_store(name: &str) -> ModelStore {
        let dir =
            std::env::temp_dir().join(format!("glance-models-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ModelStore::new(dir)
    }

    fn manifest(path: &str, format: &str, kind: &str, size: u64, sha: &str) -> Manifest {
        Manifest::parse(&format!(
            "schema = 1\n[[model]]\nid = \"m\"\nrole = \"stt\"\nlicense = \"MIT\"\nrepo = \"o/r\"\n\
             revision = \"0123456789abcdef0123456789abcdef01234567\"\n[[model.file]]\n\
             path = \"{path}\"\nkind = \"{kind}\"\nformat = \"{format}\"\nsize = {size}\nsha256 = \"{sha}\"\n"
        ))
        .unwrap()
    }

    fn write(store: &ModelStore, manifest: &Manifest, bytes: &[u8]) {
        let model = manifest.model("m").unwrap();
        let path = store.file_path(model, &model.files[0]);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn sha256_matches_the_standard_test_vector() {
        assert_eq!(hex(&Sha256::digest(b"abc")), ABC_SHA);
    }

    #[test]
    fn accepts_a_file_that_matches_its_pin() {
        let store = temp_store("ok");
        let m = manifest("vocab.txt", "text", "aux", 3, ABC_SHA);
        write(&store, &m, b"abc");
        let dir = store.verified_dir(m.model("m").unwrap()).unwrap();
        assert!(dir.ends_with("m"));
        std::fs::remove_dir_all(store.root()).unwrap();
    }

    #[test]
    fn refuses_missing_wrong_size_and_wrong_hash() {
        let store = temp_store("bad");
        let m = manifest("vocab.txt", "text", "aux", 3, ABC_SHA);
        let model = m.model("m").unwrap();
        assert!(matches!(
            store.verified_dir(model),
            Err(VerifyError::Missing { .. })
        ));
        write(&store, &m, b"abcd");
        assert!(matches!(
            store.verified_dir(model),
            Err(VerifyError::Size { .. })
        ));
        write(&store, &m, b"abd");
        assert!(matches!(
            store.verified_dir(model),
            Err(VerifyError::Hash { .. })
        ));
        std::fs::remove_dir_all(store.root()).unwrap();
    }

    #[test]
    fn refuses_a_gguf_without_the_magic_number() {
        let store = temp_store("magic");
        let bytes = b"NOPE-not-a-gguf";
        let sha = hex(&Sha256::digest(bytes));
        let m = manifest("a.gguf", "gguf", "weights", bytes.len() as u64, &sha);
        write(&store, &m, bytes);
        assert!(matches!(
            store.verified_dir(m.model("m").unwrap()),
            Err(VerifyError::Magic { .. })
        ));
        std::fs::remove_dir_all(store.root()).unwrap();
    }

    #[test]
    fn remove_deletes_only_the_listed_files() {
        let store = temp_store("remove");
        let m = manifest("sub/a.onnx", "onnx", "weights", 3, ABC_SHA);
        write(&store, &m, b"abc");
        let model = m.model("m").unwrap();
        assert_eq!(store.files_present(model), 1);
        let partial = store.partial_dir().join(format!("{ABC_SHA}.part"));
        std::fs::create_dir_all(store.partial_dir()).unwrap();
        std::fs::write(&partial, b"ab").unwrap();
        let other = store.model_dir(model).join("notes.txt");
        std::fs::write(&other, b"mine").unwrap();

        assert_eq!(store.remove(model).unwrap(), 3 + 2);
        assert_eq!(store.files_present(model), 0);
        assert!(!partial.exists());
        // The unlisted file and so its directory stay; `sub/` is gone.
        assert!(other.exists());
        assert!(!store.model_dir(model).join("sub").exists());
        std::fs::remove_file(&other).unwrap();
        // Removing again is fine; an emptied store loses the model dir.
        write(&store, &m, b"abc");
        store.remove(model).unwrap();
        assert!(!store.model_dir(model).exists());
        assert_eq!(store.remove(model).unwrap(), 0);
    }

    #[test]
    fn a_wrong_size_does_not_count_as_present() {
        let store = temp_store("present");
        let m = manifest("a.onnx", "onnx", "weights", 3, ABC_SHA);
        write(&store, &m, b"ab");
        assert_eq!(store.files_present(m.model("m").unwrap()), 0);
    }
}
