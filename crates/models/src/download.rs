//! The only code in glance that downloads anything. See `docs/DESIGN.md` §8.
//!
//! Rules enforced here:
//! - requests start at `huggingface.co` (the URL is built by [`Model::url`]),
//! - HTTPS only, no proxy, no cookies, no credentials, generic user agent,
//! - redirects are followed only to hosts on [`ALLOWED_HOSTS`],
//! - bytes are hashed while streaming into a partial file, the transfer is
//!   aborted if it exceeds the pinned size, and the file only moves into the
//!   store after it passes the same check used before every load.

use std::time::Duration;

use reqwest::redirect::Policy;
use reqwest::{Client, Url};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::manifest::{Model, ModelFile};
use crate::store::{ModelStore, VerifyError, check_file, hex};

/// Exact hosts a download may touch. `huggingface.co` answers every request
/// with a redirect to one of its own CDN hosts; the rest of this list is those
/// hosts, taken from Hugging Face's published list. Its `amazonaws.com`
/// entries are deliberately absent.
pub const ALLOWED_HOSTS: &[&str] = &[
    "huggingface.co",
    "cdn-lfs.hf.co",
    "cdn-lfs-us-1.hf.co",
    "cdn-lfs-eu-1.hf.co",
    "transfer.xethub.hf.co",
    "transfer.xethub-eu.hf.co",
    "cas-server.xethub.hf.co",
    "cas-server.xethub-eu.hf.co",
    "aws.cdn.hf.co",
    "us.aws.cdn.hf.co",
    "us-east-1.aws.cdn.hf.co",
    "us-west-2.aws.cdn.hf.co",
    "eu-west-3.aws.cdn.hf.co",
    "ap-southeast-1.aws.cdn.hf.co",
    "us.gcp.cdn.hf.co",
    "us-east1.us.gcp.cdn.hf.co",
    "us-central1.us.gcp.cdn.hf.co",
    "us-west4.us.gcp.cdn.hf.co",
    "europe-west4.us.gcp.cdn.hf.co",
    "asia-southeast1.us.gcp.cdn.hf.co",
];

const MAX_REDIRECTS: usize = 5;

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("refusing to contact `{0}`: not an allowlisted Hugging Face host over HTTPS")]
    HostNotAllowed(String),
    #[error("{url}: {source}")]
    Http { url: String, source: reqwest::Error },
    #[error("{url}: server sent more than the pinned {expected} bytes")]
    TooLarge { url: String, expected: u64 },
    #[error("downloaded file failed verification and was deleted: {0}")]
    Verify(#[from] VerifyError),
    #[error("{path}: {source}")]
    Io {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
}

/// What happened to one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    AlreadyVerified,
    Downloaded,
}

/// True when `url` is HTTPS, on the default port, with no credentials, and
/// its host is exactly one of [`ALLOWED_HOSTS`].
pub fn is_allowed_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url
            .host_str()
            .is_some_and(|host| ALLOWED_HOSTS.contains(&host))
}

fn client() -> Result<Client, reqwest::Error> {
    let redirects = Policy::custom(|attempt| {
        if attempt.previous().len() > MAX_REDIRECTS {
            attempt.error("too many redirects")
        } else if is_allowed_url(attempt.url()) {
            attempt.follow()
        } else {
            let host = attempt.url().host_str().unwrap_or("<none>").to_owned();
            attempt.error(DownloadError::HostNotAllowed(host))
        }
    });
    Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(redirects)
        .user_agent(concat!("glance/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(120))
        .build()
}

/// Download every file of `model` that is not already present and verified.
/// `progress(file, bytes_done, bytes_total)` is called as bytes arrive.
pub async fn pull(
    store: &ModelStore,
    model: &Model,
    mut progress: impl FnMut(&ModelFile, u64, u64),
) -> Result<Vec<(String, Outcome)>, DownloadError> {
    let client = client().map_err(|source| DownloadError::Http {
        url: "client setup".to_owned(),
        source,
    })?;
    let mut outcomes = Vec::new();
    for file in &model.files {
        let outcome = if store.verify_file(model, file).is_ok() {
            Outcome::AlreadyVerified
        } else {
            fetch(&client, store, model, file, &mut progress).await?;
            Outcome::Downloaded
        };
        outcomes.push((file.path.clone(), outcome));
    }
    Ok(outcomes)
}

async fn fetch(
    client: &Client,
    store: &ModelStore,
    model: &Model,
    file: &ModelFile,
    progress: &mut impl FnMut(&ModelFile, u64, u64),
) -> Result<(), DownloadError> {
    let url = model.url(file);
    let parsed = Url::parse(&url).map_err(|_| DownloadError::HostNotAllowed(url.clone()))?;
    if !is_allowed_url(&parsed) {
        return Err(DownloadError::HostNotAllowed(url));
    }
    let http = |source| DownloadError::Http {
        url: url.clone(),
        source,
    };

    let partial_dir = store.partial_dir();
    let partial = partial_dir.join(format!("{}.part", file.sha256));
    let io = |path: &std::path::Path| {
        let path = path.to_owned();
        move |source| DownloadError::Io { path, source }
    };
    tokio::fs::create_dir_all(&partial_dir)
        .await
        .map_err(io(&partial_dir))?;

    let mut response = client
        .get(parsed)
        .send()
        .await
        .map_err(http)?
        .error_for_status()
        .map_err(http)?;

    let mut out = tokio::fs::File::create(&partial)
        .await
        .map_err(io(&partial))?;
    let mut hasher = Sha256::new();
    let mut done: u64 = 0;
    while let Some(chunk) = response.chunk().await.map_err(http)? {
        done += chunk.len() as u64;
        if done > file.size {
            drop(out);
            let _ = tokio::fs::remove_file(&partial).await;
            return Err(DownloadError::TooLarge {
                url,
                expected: file.size,
            });
        }
        hasher.update(&chunk);
        out.write_all(&chunk).await.map_err(io(&partial))?;
        progress(file, done, file.size);
    }
    out.sync_all().await.map_err(io(&partial))?;
    drop(out);

    // Fast reject on the streamed hash, then the full on-disk check (size,
    // hash, magic) that every later load also goes through.
    let streamed = hex(&hasher.finalize());
    let verdict = if streamed == file.sha256 {
        let partial = partial.clone();
        let file = file.clone();
        tokio::task::spawn_blocking(move || check_file(&partial, &file))
            .await
            .expect("verification task panicked")
    } else {
        Err(VerifyError::Hash {
            path: partial.clone(),
            expected: file.sha256.clone(),
            actual: streamed,
        })
    };
    if let Err(e) = verdict {
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(e.into());
    }

    let dest = store.file_path(model, file);
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(io(parent))?;
    }
    tokio::fs::rename(&partial, &dest)
        .await
        .map_err(io(&dest))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed(url: &str) -> bool {
        is_allowed_url(&Url::parse(url).unwrap())
    }

    #[test]
    fn allows_huggingface_and_its_listed_cdn_hosts() {
        assert!(allowed(
            "https://huggingface.co/org/repo/resolve/abc/file.gguf"
        ));
        assert!(allowed("https://us.aws.cdn.hf.co/some/object?sig=1"));
        assert!(allowed("https://cdn-lfs-us-1.hf.co/x"));
    }

    #[test]
    fn refuses_everything_else() {
        for url in [
            "http://huggingface.co/x",               // not HTTPS
            "https://huggingface.co.evil.example/x", // suffix trick
            "https://evil.hf.co/x",                  // unlisted subdomain
            "https://hf.co/x",                       // not on the list
            "https://s3.us-east-1.amazonaws.com/x",  // deliberately excluded
            "https://user:pw@huggingface.co/x",      // credentials
            "https://huggingface.co:8443/x",         // odd port
            "https://127.0.0.1/x",
            "https://github.com/x",
        ] {
            assert!(!allowed(url), "{url} must be refused");
        }
    }

    #[test]
    fn every_allowlisted_host_is_a_hugging_face_domain() {
        for host in ALLOWED_HOSTS {
            assert!(
                *host == "huggingface.co" || host.ends_with(".hf.co"),
                "{host} is not a Hugging Face host"
            );
        }
    }
}
