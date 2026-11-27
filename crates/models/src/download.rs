//! The only code in glance that downloads anything. See `docs/DESIGN.md` §8.
//!
//! Rules enforced here:
//! - requests start at `huggingface.co` (the URL is built by [`Model::url`]),
//! - HTTPS only, no proxy, no cookies, no credentials, generic user agent,
//! - redirects are followed only to hosts on [`ALLOWED_HOSTS`],
//! - bytes are hashed while streaming into a partial file, the transfer is
//!   aborted if it exceeds the pinned size, and the file only moves into the
//!   store after it passes the same check used before every load.
//!
//! Resuming: an unfinished file stays in `models/.partial/<sha256>.part`
//! (a stopped, failed or interrupted download keeps it). The next attempt
//! hashes the bytes already there, then asks for the rest with a `Range`
//! header. A server that answers with the whole file (200) starts it over.
//! Whatever was on disk, the finished file must still match its pinned size,
//! SHA-256 and format before it is renamed into place, so a damaged or
//! tampered partial can only cost a retry, never be loaded. Partials can
//! never be loaded as models anyway: no manifest path can reach
//! `.partial/`, and `.part` is not an allowed extension.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use reqwest::header::{CONTENT_RANGE, RANGE};
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

/// `ENOSPC` on macOS and Linux.
const NO_SPACE: i32 = 28;

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("refusing to contact `{0}`: not an allowlisted Hugging Face host over HTTPS")]
    HostNotAllowed(String),
    #[error("{url}: {source}")]
    Http { url: String, source: reqwest::Error },
    #[error("{url}: server sent more than the pinned {expected} bytes")]
    TooLarge { url: String, expected: u64 },
    #[error("{url}: the server's partial reply did not continue where the download stopped")]
    BadRange { url: String },
    #[error("downloaded file failed verification and was deleted: {0}")]
    Verify(#[from] VerifyError),
    #[error("{path}: {source}")]
    Io {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
}

/// What went wrong, in a form a window can put into plain words. Holds no
/// URL, path or server text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// No connection, or it timed out.
    Offline,
    /// A redirect pointed outside [`ALLOWED_HOSTS`] (the host is named).
    HostRefused(String),
    /// The server answered with this HTTP status.
    Status(u16),
    /// The server sent more than the pinned size, or a broken partial reply.
    BadReply,
    /// The finished file did not match its pinned size, SHA-256 or format,
    /// and was deleted.
    Checksum,
    DiskFull,
    /// Any other local file error.
    Disk,
}

impl Failure {
    /// A short stable code for log lines.
    pub fn code(&self) -> &'static str {
        match self {
            Failure::Offline => "offline",
            Failure::HostRefused(_) => "host_refused",
            Failure::Status(_) => "http_status",
            Failure::BadReply => "bad_reply",
            Failure::Checksum => "checksum",
            Failure::DiskFull => "disk_full",
            Failure::Disk => "disk",
        }
    }
}

impl DownloadError {
    pub fn failure(&self) -> Failure {
        match self {
            DownloadError::HostNotAllowed(host) => Failure::HostRefused(host.clone()),
            DownloadError::Http { source, .. } => {
                // A refused redirect arrives wrapped in reqwest's error.
                let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(source);
                while let Some(e) = cause {
                    if let Some(DownloadError::HostNotAllowed(host)) = e.downcast_ref() {
                        return Failure::HostRefused(host.clone());
                    }
                    cause = e.source();
                }
                match source.status() {
                    Some(status) => Failure::Status(status.as_u16()),
                    None if source.is_redirect() => Failure::BadReply,
                    None => Failure::Offline,
                }
            }
            DownloadError::TooLarge { .. } | DownloadError::BadRange { .. } => Failure::BadReply,
            DownloadError::Verify(VerifyError::Io { source, .. })
            | DownloadError::Io { source, .. } => {
                if source.raw_os_error() == Some(NO_SPACE) {
                    Failure::DiskFull
                } else {
                    Failure::Disk
                }
            }
            DownloadError::Verify(_) => Failure::Checksum,
        }
    }
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
    // reqwest keeps request headers such as `Range` across redirects; it
    // drops only credentials (checked in its 0.13.5 source, redirect.rs).
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
/// `progress(file, bytes_done, bytes_total)` is called as bytes arrive (and
/// once with the bytes kept from an earlier attempt).
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
            let url = model.url(file);
            let mut source = HttpSource {
                client: &client,
                url: url.clone(),
            };
            fetch(store, model, file, &url, &mut source, &mut progress).await?;
            Outcome::Downloaded
        };
        outcomes.push((file.path.clone(), outcome));
    }
    Ok(outcomes)
}

/// A boxed future, so the test source and the HTTP source share one shape.
type Boxed<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One reply to a (possibly ranged) request.
struct Reply<'a> {
    /// 200, 206 or 416; anything else is already an error.
    status: u16,
    /// The `Content-Range` header of a 206.
    content_range: Option<String>,
    body: Box<dyn Chunks + 'a>,
}

trait Chunks: Send {
    fn next_chunk(&mut self) -> Boxed<'_, Result<Option<Vec<u8>>, DownloadError>>;
}

/// Where the bytes come from: HTTPS in glance, memory in tests.
trait Source: Send {
    /// Ask for the file from byte `start` on (`start == 0`: all of it).
    fn open(&mut self, start: u64) -> Boxed<'_, Result<Reply<'_>, DownloadError>>;
}

struct HttpSource<'c> {
    client: &'c Client,
    url: String,
}

struct HttpBody {
    response: reqwest::Response,
    url: String,
}

impl Chunks for HttpBody {
    fn next_chunk(&mut self) -> Boxed<'_, Result<Option<Vec<u8>>, DownloadError>> {
        Box::pin(async move {
            self.response
                .chunk()
                .await
                .map(|c| c.map(|b| b.to_vec()))
                .map_err(|source| DownloadError::Http {
                    url: self.url.clone(),
                    source,
                })
        })
    }
}

impl Source for HttpSource<'_> {
    fn open(&mut self, start: u64) -> Boxed<'_, Result<Reply<'_>, DownloadError>> {
        Box::pin(async move {
            let parsed = Url::parse(&self.url)
                .map_err(|_| DownloadError::HostNotAllowed(self.url.clone()))?;
            if !is_allowed_url(&parsed) {
                return Err(DownloadError::HostNotAllowed(self.url.clone()));
            }
            let http = |source| DownloadError::Http {
                url: self.url.clone(),
                source,
            };
            let mut request = self.client.get(parsed);
            if start > 0 {
                request = request.header(RANGE, format!("bytes={start}-"));
            }
            let response = request.send().await.map_err(http)?;
            let status = response.status().as_u16();
            if status == 416 {
                return Ok(Reply {
                    status,
                    content_range: None,
                    body: Box::new(Empty),
                });
            }
            let response = response.error_for_status().map_err(http)?;
            let content_range = response
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            Ok(Reply {
                status,
                content_range,
                body: Box::new(HttpBody {
                    response,
                    url: self.url.clone(),
                }),
            })
        })
    }
}

struct Empty;

impl Chunks for Empty {
    fn next_chunk(&mut self) -> Boxed<'_, Result<Option<Vec<u8>>, DownloadError>> {
        Box::pin(async { Ok(None) })
    }
}

/// How to treat a reply to a request that started at byte `start`.
#[derive(Debug, PartialEq, Eq)]
enum Continue {
    /// Append to the partial (a 206 that starts at `start`, or a full reply
    /// to a request from 0).
    Append,
    /// The server sent the whole file: truncate and start over.
    FromScratch,
    /// The server cannot serve that range (416): drop the partial and ask
    /// again from 0.
    AskAgain,
    Broken,
}

fn continue_with(status: u16, content_range: Option<&str>, start: u64, size: u64) -> Continue {
    match status {
        200 if start == 0 => Continue::Append,
        200 => Continue::FromScratch,
        206 if start > 0 => match content_range.and_then(parse_content_range) {
            Some((first, last, total)) if first == start && last + 1 == size && total == size => {
                Continue::Append
            }
            _ => Continue::Broken,
        },
        416 if start > 0 => Continue::AskAgain,
        _ => Continue::Broken,
    }
}

/// `bytes 100-199/200` → (100, 199, 200). An unknown total (`*`) is refused.
fn parse_content_range(value: &str) -> Option<(u64, u64, u64)> {
    let rest = value.trim().strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let (first, last) = range.split_once('-')?;
    let (first, last, total) = (
        first.trim().parse().ok()?,
        last.trim().parse().ok()?,
        total.trim().parse().ok()?,
    );
    (first <= last && last < total).then_some((first, last, total))
}

/// Hash the bytes already in `path`. Returns the hasher and the length;
/// `None` when there is no partial.
fn hash_existing(path: &Path) -> Result<Option<(Sha256, u64)>, DownloadError> {
    use std::io::Read;
    let mut handle = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(DownloadError::Io {
                path: path.to_owned(),
                source,
            });
        }
    };
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut len = 0u64;
    loop {
        let n = handle.read(&mut buf).map_err(|source| DownloadError::Io {
            path: path.to_owned(),
            source,
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        len += n as u64;
    }
    Ok(Some((hasher, len)))
}

async fn fetch(
    store: &ModelStore,
    model: &Model,
    file: &ModelFile,
    url: &str,
    source: &mut dyn Source,
    progress: &mut impl FnMut(&ModelFile, u64, u64),
) -> Result<(), DownloadError> {
    let partial_dir = store.partial_dir();
    let partial = store.partial_path(file);
    let io = |path: &Path| {
        let path = path.to_owned();
        move |source| DownloadError::Io { path, source }
    };
    tokio::fs::create_dir_all(&partial_dir)
        .await
        .map_err(io(&partial_dir))?;

    // What an earlier attempt left. Longer than the pin cannot be right.
    let existing = {
        let partial = partial.clone();
        tokio::task::spawn_blocking(move || hash_existing(&partial))
            .await
            .expect("hashing task panicked")?
    };
    let (mut hasher, mut done) = match existing {
        Some((_, len)) if len > file.size => {
            let _ = tokio::fs::remove_file(&partial).await;
            (Sha256::new(), 0)
        }
        Some(kept) => kept,
        None => (Sha256::new(), 0),
    };
    if done > 0 {
        progress(file, done, file.size);
    }

    if done < file.size {
        let mut asked_again = false;
        let (reply, start) = loop {
            let start = done;
            let reply = source.open(start).await?;
            match continue_with(
                reply.status,
                reply.content_range.as_deref(),
                start,
                file.size,
            ) {
                Continue::Append => break (reply, start),
                Continue::FromScratch => {
                    hasher = Sha256::new();
                    done = 0;
                    break (reply, start);
                }
                Continue::AskAgain if !asked_again => {
                    asked_again = true;
                    let _ = tokio::fs::remove_file(&partial).await;
                    hasher = Sha256::new();
                    done = 0;
                }
                Continue::AskAgain | Continue::Broken => {
                    return Err(DownloadError::BadRange {
                        url: url.to_owned(),
                    });
                }
            }
        };
        let mut body = reply.body;
        let mut out = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(done > 0)
            .truncate(done == 0)
            .open(&partial)
            .await
            .map_err(io(&partial))?;
        debug_assert!(done == 0 || done == start);
        loop {
            let chunk = match body.next_chunk().await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(e) => {
                    // Keep what arrived for the next attempt: wait for the
                    // write still in flight.
                    let _ = out.flush().await;
                    return Err(e);
                }
            };
            done += chunk.len() as u64;
            if done > file.size {
                drop(out);
                let _ = tokio::fs::remove_file(&partial).await;
                return Err(DownloadError::TooLarge {
                    url: url.to_owned(),
                    expected: file.size,
                });
            }
            hasher.update(&chunk);
            out.write_all(&chunk).await.map_err(io(&partial))?;
            progress(file, done, file.size);
        }
        out.sync_all().await.map_err(io(&partial))?;
    }

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

    let dest: PathBuf = store.file_path(model, file);
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
    use crate::manifest::Manifest;

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

    #[test]
    fn replies_to_ranged_requests_are_judged_strictly() {
        assert_eq!(continue_with(200, None, 0, 10), Continue::Append);
        assert_eq!(continue_with(200, None, 4, 10), Continue::FromScratch);
        assert_eq!(
            continue_with(206, Some("bytes 4-9/10"), 4, 10),
            Continue::Append
        );
        // Starts elsewhere, ends early, other total, unknown total, no header.
        for range in [
            Some("bytes 3-9/10"),
            Some("bytes 4-8/10"),
            Some("bytes 4-9/11"),
            Some("bytes 4-9/*"),
            None,
        ] {
            assert_eq!(
                continue_with(206, range, 4, 10),
                Continue::Broken,
                "{range:?}"
            );
        }
        assert_eq!(
            continue_with(206, Some("bytes 0-9/10"), 0, 10),
            Continue::Broken
        );
        assert_eq!(continue_with(416, None, 4, 10), Continue::AskAgain);
        assert_eq!(continue_with(416, None, 0, 10), Continue::Broken);
        assert_eq!(parse_content_range("bytes 9-3/10"), None);
    }

    /// A server in memory: the file's bytes, and whether it honours Range.
    struct Fake {
        bytes: Vec<u8>,
        ranges: bool,
        /// The `start` of every request, to check what was asked.
        asked: Vec<u64>,
        /// Stop the body after this many bytes (a dropped connection).
        cut_at: Option<usize>,
    }

    struct FakeBody(Vec<Vec<u8>>, bool);

    impl Chunks for FakeBody {
        fn next_chunk(&mut self) -> Boxed<'_, Result<Option<Vec<u8>>, DownloadError>> {
            Box::pin(async move {
                if self.0.is_empty() && self.1 {
                    return Err(DownloadError::BadRange { url: "cut".into() });
                }
                Ok((!self.0.is_empty()).then(|| self.0.remove(0)))
            })
        }
    }

    impl Source for Fake {
        fn open(&mut self, start: u64) -> Boxed<'_, Result<Reply<'_>, DownloadError>> {
            Box::pin(async move {
                self.asked.push(start);
                let size = self.bytes.len() as u64;
                let (status, from, content_range) = if start > 0 && self.ranges {
                    if start >= size {
                        (416, size, None)
                    } else {
                        (
                            206,
                            start,
                            Some(format!("bytes {start}-{}/{size}", size - 1)),
                        )
                    }
                } else {
                    (200, 0, None)
                };
                let end = self.cut_at.take().unwrap_or(self.bytes.len());
                let cut = end < self.bytes.len();
                let body = self.bytes[from as usize..end.max(from as usize)]
                    .chunks(3)
                    .map(<[u8]>::to_vec)
                    .collect();
                Ok(Reply {
                    status,
                    content_range,
                    body: Box::new(FakeBody(body, cut)),
                })
            })
        }
    }

    const CONTENT: &[u8] = b"0123456789abcdefghij";

    fn setup(name: &str, content: &[u8]) -> (ModelStore, Manifest) {
        let dir = std::env::temp_dir().join(format!("glance-dl-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sha = hex(&Sha256::digest(content));
        let manifest = Manifest::parse(&format!(
            "schema = 1\n[[model]]\nid = \"m\"\nrole = \"stt\"\nlicense = \"MIT\"\nrepo = \"o/r\"\n\
             revision = \"0123456789abcdef0123456789abcdef01234567\"\n[[model.file]]\n\
             path = \"a.onnx\"\nkind = \"weights\"\nformat = \"onnx\"\nsize = {}\nsha256 = \"{sha}\"\n",
            content.len()
        ))
        .unwrap();
        (ModelStore::new(dir), manifest)
    }

    /// Run one attempt. Progress must only go forward, unless `restarts`
    /// (a server that ignored Range sends the file again from byte 0).
    async fn run(
        store: &ModelStore,
        manifest: &Manifest,
        fake: &mut Fake,
    ) -> Result<(), DownloadError> {
        run_with(store, manifest, fake, false).await
    }

    async fn run_with(
        store: &ModelStore,
        manifest: &Manifest,
        fake: &mut Fake,
        restarts: bool,
    ) -> Result<(), DownloadError> {
        let model = manifest.model("m").unwrap();
        let mut seen = Vec::new();
        let result = fetch(
            store,
            model,
            &model.files[0],
            "test",
            fake,
            &mut |_, done, _| seen.push(done),
        )
        .await;
        let backwards = seen.windows(2).filter(|w| w[0] > w[1]).count();
        assert_eq!(backwards, usize::from(restarts), "progress: {seen:?}");
        result
    }

    fn put_partial(store: &ModelStore, manifest: &Manifest, bytes: &[u8]) {
        let file = &manifest.model("m").unwrap().files[0];
        std::fs::create_dir_all(store.partial_dir()).unwrap();
        std::fs::write(store.partial_path(file), bytes).unwrap();
    }

    fn installed(store: &ModelStore, manifest: &Manifest) -> bool {
        store.verified_dir(manifest.model("m").unwrap()).is_ok()
    }

    #[tokio::test]
    async fn an_interrupted_download_resumes_with_range() {
        let (store, manifest) = setup("resume", CONTENT);
        let mut fake = Fake {
            bytes: CONTENT.to_vec(),
            ranges: true,
            asked: vec![],
            cut_at: Some(8),
        };
        // The connection drops after 8 bytes: the partial stays.
        assert!(run(&store, &manifest, &mut fake).await.is_err());
        let file = &manifest.model("m").unwrap().files[0];
        let kept = std::fs::read(store.partial_path(file)).unwrap();
        assert!(CONTENT.starts_with(&kept) && !kept.is_empty());
        assert!(!installed(&store, &manifest));
        // The next attempt asks only for the rest and installs the file.
        run(&store, &manifest, &mut fake).await.unwrap();
        assert_eq!(fake.asked, vec![0, kept.len() as u64]);
        assert!(installed(&store, &manifest));
        assert!(!store.partial_path(file).exists());
    }

    #[tokio::test]
    async fn a_server_without_ranges_starts_over() {
        let (store, manifest) = setup("norange", CONTENT);
        put_partial(&store, &manifest, &CONTENT[..7]);
        let mut fake = Fake {
            bytes: CONTENT.to_vec(),
            ranges: false,
            asked: vec![],
            cut_at: None,
        };
        run_with(&store, &manifest, &mut fake, true).await.unwrap();
        assert_eq!(fake.asked, vec![7]);
        assert!(installed(&store, &manifest));
    }

    #[tokio::test]
    async fn an_oversized_partial_is_dropped() {
        let (store, manifest) = setup("oversize", CONTENT);
        put_partial(&store, &manifest, b"0123456789abcdefghij-extra");
        let mut fake = Fake {
            bytes: CONTENT.to_vec(),
            ranges: true,
            asked: vec![],
            cut_at: None,
        };
        run(&store, &manifest, &mut fake).await.unwrap();
        assert_eq!(fake.asked, vec![0]);
        assert!(installed(&store, &manifest));
    }

    #[tokio::test]
    async fn a_tampered_partial_fails_the_pin_and_is_deleted() {
        let (store, manifest) = setup("tampered", CONTENT);
        put_partial(&store, &manifest, b"XXXXXXX");
        let mut fake = Fake {
            bytes: CONTENT.to_vec(),
            ranges: true,
            asked: vec![],
            cut_at: None,
        };
        let err = run(&store, &manifest, &mut fake).await.unwrap_err();
        assert_eq!(err.failure(), Failure::Checksum);
        let file = &manifest.model("m").unwrap().files[0];
        assert!(!store.partial_path(file).exists());
        assert!(!installed(&store, &manifest));
        // The retry starts from nothing and succeeds.
        run(&store, &manifest, &mut fake).await.unwrap();
        assert!(installed(&store, &manifest));
    }

    #[tokio::test]
    async fn a_complete_partial_is_verified_without_a_request() {
        let (store, manifest) = setup("complete", CONTENT);
        put_partial(&store, &manifest, CONTENT);
        let mut fake = Fake {
            bytes: CONTENT.to_vec(),
            ranges: true,
            asked: vec![],
            cut_at: None,
        };
        run(&store, &manifest, &mut fake).await.unwrap();
        assert!(fake.asked.is_empty());
        assert!(installed(&store, &manifest));
    }

    #[test]
    fn failures_are_classified_without_text() {
        let host = DownloadError::HostNotAllowed("evil.example".into());
        assert_eq!(host.failure(), Failure::HostRefused("evil.example".into()));
        let full = DownloadError::Io {
            path: "/x".into(),
            source: std::io::Error::from_raw_os_error(NO_SPACE),
        };
        assert_eq!(full.failure(), Failure::DiskFull);
        let size = DownloadError::Verify(VerifyError::Size {
            path: "/x".into(),
            expected: 1,
            actual: 2,
        });
        assert_eq!(size.failure(), Failure::Checksum);
        assert_eq!(
            DownloadError::TooLarge {
                url: "u".into(),
                expected: 1
            }
            .failure()
            .code(),
            "bad_reply"
        );
    }
}
