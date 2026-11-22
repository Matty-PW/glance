# Contributing to glance

Thanks for helping. glance handles screen content and microphone audio, so
a few rules here are stricter than usual; please read [the rules](#rules)
before your first change.

## Setup

```sh
brew install llama.cpp gitleaks cargo-audit cargo-deny
git config core.hooksPath .githooks   # once per clone: enables the pre-commit hook
```

The Rust toolchain is pinned in `rust-toolchain.toml`; rustup installs it on
first build. Only the Xcode Command Line Tools are needed, not full Xcode.

The pre-commit hook refuses commits that contain secrets or home-directory
paths (gitleaks, configured in `.gitleaks.toml`), model weights, audio,
images outside `docs/images/`, env files, signing material, or any file over
2 MB.

## Build and run

```sh
cargo build                                   # everything
cargo run -p glance -- --help                 # the CLI and its tools
cargo xtask bundle                            # target/bundle/Glance.app, signed
open target/bundle/Glance.app --args app      # the app
```

Set `GLANCE_SIGN_IDENTITY` to a code-signing identity
(`security find-identity -v -p codesigning`) so macOS keeps the Screen
Recording and Microphone grants across rebuilds. Without it the bundle is
signed ad hoc and macOS asks again after each build.

## Checks

Every change must pass these (CI runs the same, plus a gitleaks scan of the
full history):

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo audit
cargo deny check
gitleaks git --redact --no-banner --config .gitleaks.toml .
```

## Rules

These come from the project's working rules (`CLAUDE.md`, which also guides
AI coding assistants used on this repo).

**Privacy**

- Screenshots and audio live in memory only: never write them to disk, logs
  or crash reports. Never log transcripts, answers or any text derived from
  the screen; log timings, counts and status codes instead.
- No telemetry, analytics or update checks. The only network traffic allowed
  is to the configured model endpoints (loopback by default) and model
  downloads from the allowlisted Hugging Face hosts.
- Model endpoints default to `127.0.0.1`. Anything non-loopback needs an
  explicit opt-in and a visible indicator (not built yet; talk to us first).
- API keys belong in the macOS Keychain, never in config files, env files or
  logs.
- Screen content is untrusted input. The model gets no tools, no shell, no
  file access and no actions; its output is only shown and spoken.

**Models**

- Never commit weights. Every model file is pinned in `models.toml` by
  Hugging Face commit, size and SHA-256, and recorded with its licence.
- Downloads start at `huggingface.co` and may only follow redirects to the
  host allowlist in `crates/models/src/download.rs`.
- Load GGUF, ONNX and safetensors only; never pickle formats
  (`.pt`, `.pth`, `.bin`). The two narrow exceptions are documented in
  `CLAUDE.md`.

**Dependencies**

- No GPL, AGPL or LGPL dependencies; `deny.toml` enforces a licence
  allowlist.
- Pin versions exactly (`=x.y.z`) and commit `Cargo.lock`.
- Explain any new dependency in the pull request: what it is, why it is
  needed, its licence, and what it pulls in.
- Never run `curl | sh` or install scripts from the internet.

**Code**

- Capture, hotkeys, audio, speech recognition, voices and the model client
  stay behind traits, so the orchestrator is testable with mocks. AppKit and
  other macOS-only code lives in `crates/macos`; no platform types reach the
  core crates.
- Log per-stage latency for anything on the path to the first spoken word.
- Small commits, one logical change each, with a clear message.
- Colours: the accent lives in `crates/macos/src/theme.rs`; use semantic
  system colours everywhere else.

## Evaluation set

`eval/` holds six mock application screens (HTML) with questions and
expected answers, so model comparisons never use real screen content:

```sh
mkdir -p eval-local/shots
for s in editor sheet mail terminal settings chart; do
  swift eval/render.swift eval/scenes/$s.html eval-local/shots/$s.png
done
cargo run --release -p glance -- fast-bench --models qwen3-vl-4b-instruct-q4km
```

Results go to the gitignored `eval-local/`.

## UI changes

`glance ui-snapshots` draws every overlay state and settings section, in
light and dark mode and at the minimum window size, with demo text, to
`eval-local/ui/`. It renders glance's own views in-process and never
captures the screen. Please look at the snapshots for any UI change, and
check Reduce Motion and Reduce Transparency (System Settings →
Accessibility → Display) by hand.

## Recording a demo

No special mode is needed. glance leaves its own windows out of the
screenshots it sends to the model only (a ScreenCaptureKit filter on
glance's own process); it does not hide them from other apps. So QuickTime
Player, OBS or `screencapture` record the overlay normally, while the model
still never sees it.

- `open target/bundle/Glance.app --args app --overlay-demo` plays two
  scripted turns (fixed text, nothing is captured or asked) to film the
  overlay's states.
- For a real turn, record the whole screen while you ask.
- Don't record private content; use the mock screens in `eval/scenes/`.
- TODO (maintainer): confirm with a test recording that the overlay shows
  up in QuickTime and OBS.

## Pull requests

Fill in the pull request template: what changed, why, how you tested it,
and confirmation that the checks pass and the privacy rules hold. For
security problems, don't open an issue or pull request; see
[SECURITY.md](SECURITY.md).
