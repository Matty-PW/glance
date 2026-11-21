# glance

**Ask your Mac about what's on screen, out loud, and hear the answer. Everything stays on your machine.**

Hold a hotkey, ask a question ("what's this error?", "how much did we spend
in August?"), let go. glance takes a screenshot, transcribes your question,
asks a local vision model, and speaks the answer back while it streams onto a
small overlay. For harder questions, *think harder* hands the same question
and screenshot to a bigger local model.

- **Local only.** Speech recognition, the vision models and the voice all run
  on your Mac. No telemetry, no analytics, no update checks, no cloud.
- **Fast.** The first spoken word comes about 1.1 s after you let go of the
  key (4B model on an M4 Pro).
- **Native.** A menu-bar app with an overlay that never takes focus and a
  native settings window, written in Rust.

> **Status:** in development, macOS on Apple Silicon only. Milestones M0 to
> M5 are built (see [Roadmap](#roadmap)); the hands-on acceptance runs are
> still open. There are no binary releases.

---

## Contents

- [How it works](#how-it-works)
- [Privacy and security](#privacy-and-security)
- [Requirements](#requirements)
- [Getting started](#getting-started)
- [Using glance](#using-glance)
- [Settings](#settings)
- [Models](#models)
- [Command line](#command-line)
- [Development](#development)
- [Roadmap](#roadmap)
- [Licence](#licence)

## How it works

```
hold Ctrl+Opt+G ──► screenshot (ScreenCaptureKit) ──► image prefilled into FAST
speak           ──► microphone, in memory only
release         ──► speech-to-text (Parakeet) ──► FAST vision model (llama-server)
                                                   │ streams sentences
                                                   ▼
                          overlay text  ◄──  voice (Kokoro or the macOS voice)
```

| Part | What runs | Where |
|---|---|---|
| Speech-to-text | Parakeet TDT 0.6B v3 (int8 ONNX) | in-process |
| FAST model | Qwen3-VL 4B Instruct (Q4_K_M GGUF) | `llama-server` on 127.0.0.1, started by glance |
| DEEP model (optional) | Qwen3.8 27B (Q4_K_M GGUF), loaded only when asked | `llama-server` on 127.0.0.1 |
| Voice | Kokoro-82M (fp32 ONNX), or the macOS system voice | in-process |

The screenshot is sent to the model while you are still talking, so answering
starts the moment you let go. Answers are spoken sentence by sentence as they
arrive.

## Privacy and security

These are design rules, enforced in code (see [CLAUDE.md](CLAUDE.md) for the
full list):

- **Screenshots and audio stay in memory.** They are never written to disk,
  logs or crash reports.
- **No network traffic** except to the model servers on 127.0.0.1 and model
  downloads from Hugging Face. glance's own servers bind to 127.0.0.1 on a
  random port with a per-launch key.
- **Pinned downloads.** Every model file is pinned by commit and SHA-256 in
  [`models.toml`](models.toml), downloaded over HTTPS from `huggingface.co`
  (redirects only to an exact list of Hugging Face CDN hosts), and hashed
  again before use. Only GGUF, ONNX and safetensors are loaded, never pickle
  formats.
- **Screen content is untrusted.** The model has no tools, no shell and no
  file access; its output is only shown and spoken.
- **Licences.** No GPL-family dependencies (enforced by `deny.toml`). Model
  licences are listed per model; special cases are in
  [THIRD_PARTY.md](THIRD_PARTY.md).

## Requirements

- A Mac with Apple Silicon, macOS 14.2 or later. The default setup uses about
  7 GB of memory; the optional DEEP model needs about 20 GB more.
- Xcode Command Line Tools (full Xcode is not needed).
- [rustup](https://rustup.rs). The toolchain version is pinned in
  `rust-toolchain.toml` and installed on first build.
- `llama-server` from llama.cpp: `brew install llama.cpp`.
- About 4.5 GB of disk for the default models (another 18 GB for DEEP).

## Getting started

**1. Build and download the models**

```sh
git clone <this repository> glance && cd glance
alias glance='cargo run --release -q -p glance --'

glance models pull qwen3-vl-4b-instruct-q4km    # FAST vision model, 3.3 GB
glance models pull parakeet-tdt-0.6b-v3-int8    # speech recognition, 0.7 GB
glance models pull kokoro-82m-v1.0-fp32         # voice, 0.3 GB
glance models pull kokoro-82m-v1.0-voices-en    # 28 English voices
```

Every file is checked against its pinned SHA-256 before it is stored.

**2. Build the app**

```sh
security find-identity -v -p codesigning        # pick a signing identity
export GLANCE_SIGN_IDENTITY=<its SHA-1>
cargo xtask bundle                              # builds and signs target/bundle/Glance.app
```

With a stable signing identity, macOS keeps your permission grants across
rebuilds. Without one, glance is signed ad hoc and macOS asks again after
every build.

**3. Run it**

```sh
open target/bundle/Glance.app --args app
```

Or, to see its log in your terminal:

```sh
open -W --stderr "$(tty)" target/bundle/Glance.app --args app
```

On first use macOS asks for **Screen Recording** and **Microphone** access;
the menu-bar menu shows what is missing and links to System Settings. glance
starts its model server itself, and stops it when you quit. If it ever
crashes, the next start cleans up the leftover server.

To see the overlay without asking anything, add `--overlay-demo`; it plays
through every state twice.

## Using glance

| Hotkey | Action |
|---|---|
| **Hold** `⌃ ⌥ G` | Ask about the screen. Follow-up questions reuse the same screenshot. |
| **Hold** `⌃ ⌥ ⇧ G` | Take a fresh screenshot, then ask. |
| **Tap** `⌃ ⌥ T` | *Think harder:* ask DEEP the last question again, same screenshot. |
| **Hold** `⌃ ⌥ T` | Ask DEEP something new about the same screenshot. |

All hotkeys can be changed in Settings. Pressing any hotkey while glance is
talking stops it and starts listening.

- **Follow-ups.** A session keeps the screenshot and the last 12 questions, so
  you can ask "and in September?". It ends after 10 minutes idle, or with
  **New session** in the menu-bar menu.
- **Think harder.** Saying "think harder" also works. DEEP loads on demand
  (glance says so while it loads) and unloads after 10 minutes unused.
- **More, or less.** "Tell me more" or "go on" continues where the answer
  stopped. "Walk me through…" or "in detail" always gets the full answer.
- **The overlay** sits in the top-right corner without taking focus, and you
  can click through it. It shows the state (Ready, Listening with a live mic
  waveform, Thinking, Speaking), your question, and the answer as it is
  spoken, then fades out. It follows light and dark mode, Reduce Motion and
  Reduce Transparency.

## Settings

Open **Settings…** from the menu-bar icon. Most changes apply straight away;
rows marked *Restart to apply* take effect after a restart, and each section
has *Restore defaults…*.

| Section | What you can change |
|---|---|
| Voice | Kokoro or the macOS voice, the voice, speed, saying maths symbols as words ("2 × 2 = 4" → "2 times 2 equals 4") |
| Answers | Answer length: short, medium or long |
| Personality | Neutral, friendly, direct, strict, three fun ones (pirate, caveman, Shakespearean) or your own style. Personality changes how answers sound, never how long they are. |
| Think harder | DEEP's reasoning effort, when to unload it |
| Models | FAST and DEEP model, DEEP's thinking cap, automatic hand-over; download, check, repair and delete pinned models |
| Hotkeys | The three chords. A new chord only takes effect if macOS lets glance register it; otherwise the old one stays. |
| Appearance | Accent colour: glance's coral or your macOS accent |
| About | Version, model licences, third-party notices |

Everything is stored in `~/Library/Application Support/glance/config.toml`,
which you can also edit by hand. The settings window changes only the keys it
owns and keeps your comments. An invalid file is reported with the problem
named, never silently ignored.

<details>
<summary>All config options</summary>

```toml
[speech]
engine = "kokoro"        # or "system" (the macOS voice)
kokoro_model = "fp32"    # or "q8f16" (smaller, about 3x slower)
voice = "af_heart"       # see `glance voices`; a blend: "af_heart:0.6,bf_emma:0.4"
speed = 1.0              # Kokoro pace, 0.5 to 2.0
volume = 1.0             # 0.0 to 1.0
threads = 10             # ONNX Runtime threads for Kokoro
system_voice = "en-GB"   # for engine = "system": identifier or language
system_rate = 0.5        # 0.0 (slow) to 1.0 (fast)
math_as_words = false    # say "2 x 2 = 4" as "2 times 2 equals 4" (speech only)

[models]
fast = "qwen3-vl-4b-instruct-q4km"  # a vision model from models.toml
deep = "qwen3.8-27b-q4km"           # optional; leave out for FAST only (the default)
auto_escalate = false               # let FAST hand hard questions to DEEP by itself
deep_idle_secs = 600                # unload DEEP after this long unused
response_length = "medium"          # short | medium | long
personality = "neutral"             # neutral | friendly | direct | strict |
                                    # pirate | caveman | shakespearean | custom
personality_custom = ""             # for "custom": up to 300 characters
fast_ctx = 8192
deep_ctx = 16384
deep_effort = "medium"              # off | low | medium | xhigh
deep_thinking_budget = 128          # cap on DEEP's thinking tokens; 0 = no cap
deep_mtp = true                     # speculative decoding with the model's MTP layers
# Your own servers instead (loopback only):
# fast_endpoint = "http://127.0.0.1:8080/v1"
# deep_endpoint = "http://127.0.0.1:8081/v1"

[overlay]
enabled = true

[appearance]
accent = "glance"                   # or "system"

[hotkeys]                           # Ctrl, Opt, Shift, Cmd, then one key
talk = "Ctrl+Opt+G"
look = "Ctrl+Opt+Shift+G"
think = "Ctrl+Opt+T"
```

</details>

## Models

All models are pinned in [`models.toml`](models.toml) and managed from
**Settings → Models** or `glance models`.

| Model | Role | Size | Licence |
|---|---|---|---|
| `qwen3-vl-4b-instruct-q4km` | FAST (default) | 3.3 GB | Apache-2.0 |
| `qwen3-vl-8b-instruct-q4km` | FAST, larger | 6.2 GB | Apache-2.0 |
| `lfm2.5-vl-3b-q4km` | FAST, alternative | 2.5 GB | LFM Open License v1.0 |
| `qwen3.8-27b-q4km` | DEEP (think harder) | 18.4 GB | Apache-2.0 |
| `parakeet-tdt-0.6b-v3-int8` | Speech-to-text (default) | 0.7 GB | CC-BY-4.0 |
| `parakeet-tdt-0.6b-v2-int8` | Speech-to-text, English only | 0.7 GB | CC-BY-4.0 |
| `kokoro-82m-v1.0-fp32` | Voice (default) | 0.3 GB | Apache-2.0 |
| `kokoro-82m-v1.0-q8f16` | Voice, smaller | 0.1 GB | Apache-2.0 |
| `kokoro-82m-v1.0-voices-en` | 28 English voices | 0.01 GB | Apache-2.0 |

Measured on an M4 Pro (48 GB):

| | FAST 4B | FAST 8B | DEEP 27B (think harder) |
|---|---|---|---|
| Memory, with speech and voice | 7.0 GB | 9.8 GB | about +19 GB |
| Key-up to first spoken word | ~1.1 s | ~1.1 s | ~10 s once loaded; ~49 s from cold |
| Ordinary questions correct (eval set) | 12/12 | 12/12 | — |

Details and methods are in [docs/PROGRESS.md](docs/PROGRESS.md).

## Command line

The app is one subcommand of the `glance` binary; the rest are tools for
checking and measuring.

| Command | What it does |
|---|---|
| `glance app` | The menu-bar app (`--overlay-demo`, `--open-settings`, `--open-models`) |
| `glance models list \| pull <id> \| verify [id]` | Pinned models: status, download, re-hash |
| `glance say "text"` | Speak text with the current voice (`--voice`, `--speed`, `--engine`) |
| `glance voices` | List the Kokoro voices and English system voices |
| `glance serve <id>` | Verify a vision model and run `llama-server` for it on 127.0.0.1 |
| `glance mic-check`, `glance capture-check` | Check the microphone and screen capture (nothing is kept) |
| `glance bench`, `fast-bench`, `deep-bench` | Run the evaluation set against the models |
| `glance stt-bench`, `tts-bench` | Measure speech recognition and voices |
| `glance ui-snapshots` | Draw glance's own windows to PNGs for layout review |

Run any command with `--help` for its options.

## Development

**Setup**

```sh
brew install gitleaks cargo-audit cargo-deny
git config core.hooksPath .githooks   # once per clone: enables the pre-commit hook
```

The pre-commit hook refuses secrets, home-directory paths, model weights,
audio, images and large files.

**Checks** (CI runs the same, plus a full-history secret scan)

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo audit
cargo deny check
```

**Layout**

| Crate | Role |
|---|---|
| `orchestrator` | The turn state machine, sessions, escalation, settings types; platform-free and tested with mocks |
| `llm-client` | OpenAI-compatible streaming client, managed `llama-server`, escalation gate |
| `stt`, `tts`, `audio`, `capture` | Speech-to-text, voices, microphone and speaker, capture traits |
| `models` | Manifest, pinned downloader, verification |
| `macos` | Everything macOS: ScreenCaptureKit, hotkeys, system voice, menu bar, overlay, settings window, app bundle |
| `app` | The `glance` binary: config, wiring, command-line tools |
| `xtask` | `cargo xtask bundle` |

Capture, hotkeys, audio, speech-to-text, voices and the model client sit
behind traits, so the core crates contain no AppKit code.

**Evaluation set.** `eval/` holds six mock application screens (HTML) with
questions and expected answers, so model comparisons never use real screen
content:

```sh
mkdir -p eval-local/shots
for s in editor sheet mail terminal settings chart; do
  swift eval/render.swift eval/scenes/$s.html eval-local/shots/$s.png
done
glance fast-bench --models qwen3-vl-4b-instruct-q4km
```

**UI review.** `glance ui-snapshots` draws every overlay state and settings
section in light and dark mode, and at the minimum window size, with demo
text, to `eval-local/ui/`. It never captures the screen. `eval-local/` is
gitignored.

## Roadmap

| Milestone | Status |
|---|---|
| M0: command-line spike (record, transcribe, ask, time) | Done |
| M1: voice with sentence streaming | Done (Kokoro default, macOS voice as an option) |
| M2: hotkey, live capture, app bundle, permissions | Built; hands-on acceptance open |
| M3: follow-ups, fresh look vs ask, session expiry | Built; spoken run open |
| M4: escalation, managed servers, DEEP | Built; DEEP chosen and measured (Qwen3.8 27B) |
| M5: overlay, settings, model management, redesign | Built; hands-on checks open |
| Next | Remote endpoints with the macOS Keychain (with a security review) |

The design is in [docs/DESIGN.md](docs/DESIGN.md), and status, measurements
and open items in [docs/PROGRESS.md](docs/PROGRESS.md).

## Licence

glance's own licence has not been chosen yet. Dependencies are limited to
permissive licences by `deny.toml`. Model licences are listed above and in
`models.toml`, and third-party caveats (notably the phonemizer's dictionary
data) in [THIRD_PARTY.md](THIRD_PARTY.md).
