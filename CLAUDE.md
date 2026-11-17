# glance

Local-first macOS app (Rust): hold a hotkey, capture the screen, talk to a local
vision model about it, hear the answer spoken back. Dev machine: MacBook Pro
M4 Pro, 48GB.

## Security and privacy rules (non-negotiable)
- Screenshots and audio live in memory only. Never write them to disk, logs, or
  crash reports. A `--debug-dump` flag may save them, off by default, and must
  print a visible warning when on.
- No telemetry, analytics, or update pings. The only network traffic allowed:
  (a) the user-configured model endpoints, (b) model downloads from the
  allowlisted hosts below.
- Model endpoints default to loopback (127.0.0.1) only. Any non-loopback or
  cloud endpoint requires explicit opt-in in settings and shows a persistent
  "screen is being sent to <host>" indicator.
- Any llama-server we spawn binds to 127.0.0.1 on a random port.
- API keys go in the macOS Keychain, never in config files, env files, or logs.
- Screen content is untrusted input (prompt injection). v1 has NO tools, no
  shell, no file access, and no actions. Model output is only ever displayed
  and spoken. The escalate signal is parsed strictly from a fixed schema.
- Model weights are never committed to git. They download to
  ~/Library/Application Support/glance/models/ over HTTPS. Every download
  starts at huggingface.co and may follow redirects only to an exact allowlist
  of Hugging Face-operated `*.hf.co` CDN hosts (list in code). Files must match
  a pinned SHA-256 in `models.toml` before use. Accept GGUF, ONNX, and
  safetensors only. Never load pickle-based formats (.pt, .pth, .bin). Two
  narrow exceptions, both SHA-256-pinned in `models.toml` like any other file:
  Kokoro voice files (`raw-f32` .bin, read as plain little-endian floats with a
  length check, no deserializer) and plain-text aux files (vocab, config,
  tokenizer; parsed as text or JSON only). Record each model's license in
  `models.toml`.
- No GPL, AGPL, or LGPL dependencies (so no espeak-ng). `deny.toml` enforces
  this with a licence allowlist.
- Never run `curl | sh` or install scripts from the internet. Tell me what a
  dependency or download is and why before adding it.

## Repo hygiene
- `.gitignore` must cover: models/, *.gguf, *.onnx, *.safetensors, *.wav,
  screenshots/, *.png from debug dumps, .env*, target/, .DS_Store. Create it
  before the first commit.
- Commit Cargo.lock. Pin dependency versions. Run `cargo audit` and
  `cargo deny check` before each milestone is called done.
- Never commit secrets or personal paths. Run `gitleaks` (or equivalent) as a
  pre-commit hook.
- Commit after each crate or logical unit of work, not only at the end of a
  milestone, each with a clear message. NEVER push. I push.

## Working rules
- Before using any crate, check its current docs on docs.rs. Don't rely on memory
  for APIs.
- Keep capture, hotkey, audio, STT, TTS, and LLM client behind traits so the
  orchestrator is testable with mocks and other platforms can be added later.
- Log per-stage latency (capture, STT, time-to-first-token, time-to-first-audio)
  from M0 onward. Time-to-first-audio is the metric that matters.
- Explain non-obvious choices briefly. I'm comfortable with Rust basics but new
  to audio, ML inference and GUI work in Rust. Ask before big architectural
  decisions.
- Work on one milestone at a time. Use subagents only for scoped, read-only
  research. Don't parallelize edits to the same crate.

## Current state (read before starting work)
- `docs/PROGRESS.md` is the source of truth for status, measurements, open
  items and the next step. `docs/DESIGN.md` §14 lists the milestones.
- M0 and M1 are done. M2's code is done (orchestrator turn loop, `glance
  app`, permissions, `cargo xtask bundle`, `capture-check`); its acceptance
  run (signed bundle, chord presses, grants surviving rebuilds) needs me.
- M3's code is done too (sessions: follow-ups, `look` vs `talk`, idle
  expiry, "New session"); it needs a spoken run through the app. M4 is next.
- TTS is the macOS system voice through M4. Kokoro is parked; don't resume it
  unless asked (what's left is in `docs/PROGRESS.md`).
- `deny.toml` ignores RUSTSEC-2026-0542 (`humantime`, via `transcribe-rs`),
  with my OK; remove it when transcribe-rs drops env_logger 0.10. Any other
  advisory ignore still needs my OK first.
- Builds target macOS 14.2 (`.cargo/config.toml`). Only the Command Line Tools
  are installed, not full Xcode; `crates/capture/build.rs` covers the Swift
  link path for that.
- `ort` is held at 2.0.0-rc.12 because `transcribe-rs` 0.3.12 needs it.