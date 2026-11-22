# Security policy

glance looks at your screen and listens to your microphone, so it is built
to keep both on your Mac. This page says what that means in practice and how
to report a problem.

## What stays local

- **Screenshots and microphone audio live in memory only.** They are never
  written to disk, logs or crash reports, and are sent only to glance's own
  model server on `127.0.0.1`. glance's own windows (the overlay, settings)
  are left out of every screenshot.
- **Transcripts, answers and screen text are never logged.** Log lines
  hold timings, counts, status codes and fixed messages. Errors are built
  so they can't carry content: a voice error keeps only the sentence's
  length, and a model error keeps only the HTTP status and a message from a
  fixed list (never the server's text); tests check both. Logs can contain
  local file paths, which show your username.
- **No telemetry**, analytics, crash reporting or update checks.

## Network rules

- **Model servers:** glance starts `llama-server` bound to `127.0.0.1` on a
  random free port, with an API key generated for each launch and passed in
  the server's environment (never on the command line). llama-server runs
  with `--offline`, so it downloads nothing. Endpoints you configure
  yourself must also be loopback addresses.
- **Model downloads are the only network access at run time.** They happen
  only when you ask (`glance models pull`, or Settings → Models), go to
  `huggingface.co` over HTTPS, and follow redirects only to an exact list of
  Hugging Face CDN hosts (in `crates/models/src/download.rs`). Every file
  is pinned by commit, size and SHA-256 in `models.toml`, checked while it
  downloads, and checked again before it is loaded. Only GGUF, ONNX and
  safetensors weights are accepted; pickle formats are never loaded.
- **Build time:** Cargo downloads crates from crates.io, and the `ort-sys`
  build script downloads ONNX Runtime from `cdn.pyke.io`, checked against a
  SHA-256 shipped in the crate.
- **Enforced in the build:** `deny.toml` allows an HTTP client only in the
  downloader and model-client crates.

## Untrusted screen content

Anything on screen can contain text written to manipulate a model (prompt
injection). glance therefore gives the model **no tools, no shell, no file
access and no actions**: its output is only shown and spoken. The one
structured signal it may send (handing a question to the bigger model) is
parsed strictly against a fixed schema and can only trigger that hand-over.

## Supported versions

glance is pre-release; only the latest commit on `main` is supported.

## Reporting a vulnerability

Please **do not open a public issue** for a security problem.

- Use GitHub's private vulnerability reporting: the **Security** tab of this
  repository → **Report a vulnerability**. This is the only channel; there
  is no security email address.

Include what you found, how to reproduce it, the glance commit, your macOS
version and Mac model. Please don't include screenshots or recordings of
private screen content. You can expect an acknowledgement within usually a week or two.
