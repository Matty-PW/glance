# glance — design

Status: approved design for v1, written 2026-11-16. Read together with `CLAUDE.md`,
whose security and privacy rules take precedence over anything here.

Crate versions and model file sizes were read from docs.rs, Hugging Face, and
upstream sources on the date above. Anything not confirmed from a primary source is
listed in §13 with the milestone that settles it.

## 0. Decisions and deviations from the original brief

Decided:
1. Model downloads start at `huggingface.co` and may follow redirects only to an
   exact, hard-coded list of HF-operated `*.hf.co` hosts (§8).
2. Kokoro voices load under a narrow `raw-f32` exception to the "no `.bin`" rule (§8).
3. No GPL code: pure-Rust phonemizer, no espeak-ng (§6).
4. Push-to-talk is a key chord registered as a Carbon hotkey, so v1 needs no Input
   Monitoring permission (§10).
5. UI toolkit: egui (§9).

Deviations from the brief:
- **Screen capture uses the `screencapturekit` crate, not `xcap`.** xcap 0.9.10
  captures with `CGWindowListCreateImage` (legacy CoreGraphics path) and does not
  use ScreenCaptureKit at all.
- **STT is Parakeet on ONNX Runtime, which is CPU, not Metal.** Only whisper.cpp is
  Metal-accelerated, and its models are GGML `.bin` files, which the format rule
  bans (§5).
- **Two extra crates** beyond the seven listed: `models` and `xtask` (§2).
- **TTS starts with the macOS system voice**, so later milestones do not depend on
  Kokoro landing (§6).
- **M2 includes a minimal tray icon** (status + Quit). A Dock-less app needs a way
  to quit and show state; overlay and settings still wait for M5.
- **ONNX Runtime comes from Homebrew** if it passes a dependency check, otherwise
  from the `ort` crate's checksum-verified download (§6.4).
- **CLAUDE.md amendments** recording decisions 1–3, plus "plain-text aux files
  (vocab/config) are allowed when hash-pinned". Exact diff proposed in Step 2.

## 1. Goals and non-goals

v1: macOS only (minimum 14.2, set by cpal 0.18), Apple Silicon. Push-to-talk voice
Q&A about a screenshot, FAST model by default with strict escalation to DEEP,
spoken replies streamed per sentence. Time-to-first-audio (TTFA) is the metric
that matters.

Not in v1: wake word, always-listening, barge-in, tools or actions of any kind,
cloud by default, hardware presets, other platforms, on-disk history, App Sandbox.

## 2. Workspace layout

```
Cargo.toml            workspace; [workspace.dependencies] with exact (=x.y.z) pins
rust-toolchain.toml   pinned stable toolchain, edition 2024
models.toml           pinned model manifest (compiled into the binary)
deny.toml  .gitleaks.toml  .githooks/pre-commit  .github/workflows/ci.yml
crates/
  capture/       trait ScreenCapture      + ScreenCaptureKit impl
  audio/         traits AudioInput/Output + cpal impl, resampling, ring buffers
  stt/           trait SpeechToText       + Parakeet impl
  tts/           trait TextToSpeech, SentenceChunker
                 + system-voice impl, + Kokoro impl (trait Phonemizer)
  llm-client/    trait ChatModel          + OpenAI-compatible SSE client,
                 escalation parser, managed llama-server supervisor
  models/        manifest parser, downloader, verifier   (extra crate)
  orchestrator/  turn state machine, session, config types, trait HotkeySource
  app/           binary: main-thread event loop, hotkey/tray impls, CLI, later UI
  xtask/         `cargo xtask bundle`: .app assembly + codesign   (extra crate)
```

Why `models` is separate: it is the only code that downloads anything, so it gets
its own small dependency set and its own tests. Only `models` and `llm-client` may
depend on an HTTP client; CI enforces this with `cargo deny` bans.

Traits are object-safe (`Arc<dyn …>`), so the orchestrator is built entirely from
mocks in tests. `Screenshot` and `AudioClip` are opaque in-memory types: no
`Serialize`, no `Display`, and a redacted `Debug` (`Screenshot(1512x982, 380 kB)`),
so they cannot reach a log or a file by accident.

Threads: the main thread runs the AppKit/winit event loop (required by the hotkey
and tray crates). tokio's multi-thread runtime runs beside it. STT and Kokoro
inference are blocking and CPU-heavy, so each owns a dedicated OS thread fed by
channels. cpal callbacks run on CoreAudio's high-priority thread and only touch
lock-free ring buffers.

## 3. Crate choices

| Area | Choice (version) | Licence | Why / notes |
|---|---|---|---|
| Async | tokio | MIT | as specified |
| Screen capture | `screencapturekit` 11.1.2, feature `macos_14_0` | MIT/Apache | `SCScreenshotManager::capture_image(&filter, &config)` one-shot capture. `xcap` rejected (§0) |
| Hotkey | `global-hotkey` 0.10.0 | MIT/Apache | Carbon `RegisterEventHotKey`; source confirms it emits `Pressed` and `Released`. Needs a real key (no bare modifier). Must be created on the main thread |
| Audio I/O | `cpal` 0.18.2 | Apache | as specified; macOS 14.2+ |
| Resampling | `rubato` 5.1.0 | MIT/Apache | mic rate → 16 kHz; 24 kHz → output device rate |
| STT | `transcribe-rs` 0.3.12, feature `onnx` | MIT | §5 |
| ONNX Runtime | `ort` 2.0.0-rc.15 (ONNX Runtime 1.31 API) | MIT/Apache | shared by STT and Kokoro; still a release candidate, no stable 2.0 exists |
| System voice | AVSpeechSynthesizer through objc2 bindings | MIT/Apache | §6.2; exact crate confirmed on docs.rs before use |
| Phonemes | `misaki-rs` 0.9.0, `default-features = false` | MIT | pure Rust, embedded en-US/en-GB lexicon; its default `espeak` feature (GPL) stays off |
| LLM HTTP | `reqwest` (rustls) + hand-written SSE parsing | MIT/Apache | needs non-standard fields (`timings`, `chat_template_kwargs`, `grammar`) and a strict network policy; `async-openai` hides both |
| Tray | `tray-icon` 0.26.1 | MIT/Apache | same event-loop model as global-hotkey |
| UI (M5) | `eframe`/`egui` 0.37.0 (winit 0.30) | MIT/Apache | §9 |
| Keychain | `keyring` 4.2.0 (or `security-framework` 3.7.0 directly) | MIT/Apache | decided at M5 after reading its feature docs |
| Hashing | `sha2` | MIT/Apache | manifest verification |
| Bundling | own `xtask` | — | `cargo-bundle` calls itself "very early alpha" and signs from a .p12 file; `cargo-packager` last released a year ago. An xtask is ~100 lines and no new dependency |

Not yet read on docs.rs (will be, before first use, per CLAUDE.md): tokio, reqwest,
serde/toml, sha2, image, tracing, clap, base64, tokio-util, the objc2 AVFAudio
bindings.

Native/CLI tools, installed with Homebrew only after being named and agreed:
`llama.cpp` (provides `llama-server`; formula 1.0.0, Apple Silicon bottles),
`onnxruntime` (1.32.0, MIT; subject to §6.4), `gitleaks`, `cargo-audit`,
`cargo-deny`. No `curl | sh` anywhere.

## 4. Data flow

```
 key down ─► capture active display ─► downscale + PNG (memory) ─┬─► FAST prewarm (image only)
          └► mic stream on ─► ring buffer                          │
 key up ──► mic off ─► resample 16 kHz ─► STT ─► transcript ──────┴─► FAST chat (stream)
                                                                        │
                    first char '{' ? ── yes ─► strict parse ─► escalate ─► filler line
                          │ no                                            └► DEEP chat (stream)
                          ▼                                                     │
              token deltas ─► SentenceChunker ─► TextToSpeech ─► speaker ◄──────┘
```

States: `Idle → Listening → Transcribing → Thinking → Speaking → Idle`, with
`Escalating` between Thinking and Speaking. Every turn carries a cancellation
token. Pressing the hotkey in any non-idle state cancels the turn (stops audio,
aborts the HTTP stream) and starts listening. That is not barge-in: the mic is
still only open while the key is held. Presses shorter than 250 ms only cancel.
Recording is capped at 60 s.

**Hotkeys** (defaults, configurable): `talk` = Ctrl+Opt+G, `look` =
Ctrl+Opt+Shift+G. Ctrl+Opt+Space is avoided because macOS uses it for input-source
switching.
- `talk` held: if the session has no screenshot, capture then record; otherwise
  record only (follow-up on the same screenshot).
- `look` held: always capture a fresh screenshot, then record. History is kept;
  the previous image is replaced in history by a short text placeholder.
- A session ends from the tray ("New session") or after 10 idle minutes. History
  and screenshot live in memory only and are gone at quit.
- The overlay shows the screenshot's age, so a stale capture is visible.
- Registration is checked at startup. If either chord fails to register, glance
  reports an error naming the chord (stderr and tray) instead of running without a
  working hotkey. See M2 in §14 for the acceptance check.

**Active display** = the display holding the frontmost app's front window, falling
back to the display under the pointer. The capture filter excludes glance's own
windows so the overlay never appears in a screenshot.

**Image handling.** One capture, downscaled per tier (FAST long edge ≤ 1280 px,
DEEP ≤ 2048 px, both configurable), PNG with fast compression, base64 data URI.
Never a remote URL: llama-server would fetch it.

**Prompt layout and prewarm (the main latency lever).** Messages are
`[system, user:[image, text], assistant, user:text, …]` with the image first. At
key-down glance sends FAST a one-token request containing only system + image.
llama-server caches the prompt prefix by default (`cache_prompt`), so by the time
the user stops talking the image is already processed and the real request only
has to evaluate the transcript. Follow-ups are always warm. DEEP is prewarmed after
FAST finishes its prewarm; whether that slows FAST too much is measured in M4.
This whole mechanism depends on llama-server reusing a cached image prefix, which
is the first thing M0 verifies.

**Speech out.** `SentenceChunker` splits on sentence ends (guarding decimals and
abbreviations); the first chunk may flush early at a clause boundary to cut TTFA.
Text is stripped of markdown and URLs before it is spoken.

**Thinking models.** FAST runs with thinking off. DEEP may think; `reasoning_content`
deltas are never spoken, only shown as "thinking". Per-tier request extras
(`chat_template_kwargs`, sampling) come from a free-form `extra_body` table in
settings, which keeps this portable across llama-server, Ollama, and LM Studio.

## 5. STT recommendation: Parakeet TDT 0.6b (int8 ONNX) via `transcribe-rs`

| Option | Runtime / accel | Model format | Verdict |
|---|---|---|---|
| **Parakeet TDT 0.6b** (`transcribe-rs`, `onnx`) | ONNX Runtime, CPU (CoreML optional) | ONNX dir, 652 MB + 18 MB int8 | **Recommended** |
| Moonshine (`transcribe-rs`, `onnx`) | ONNX Runtime, CPU | ONNX | Low-RAM alternative; English only, less accurate |
| Whisper (`whisper-rs` 0.16 / `transcribe-rs` `whisper-metal`) | whisper.cpp, Metal | GGML `.bin` | Blocked by the `.bin` rule; fixed 30 s encoder window costs latency on short clips |
| `parakeet-rs` 0.4.4 | ONNX Runtime, pins `ort =2.0.0-rc.15` | ONNX dir | Fallback if transcribe-rs disappoints |

Reasons:
1. Speed: transcribe-rs's README reports ~30x real time for Parakeet int8 on an
   M4 Max CPU, so a 5 s clip is roughly 0.2 s, with no fixed 30 s window.
2. Format: ONNX is on the allowed list, Whisper's GGML is not.
3. One native dependency: it shares ONNX Runtime with Kokoro.
4. transcribe-rs puts Parakeet and Moonshine behind one API and is what the Handy
   dictation app ships.

Caveat: this is CPU, not Metal. If the M0 spike misses the budget, Whisper and the
format rule get revisited with the maintainer before anything changes.

The M0 spike compares Parakeet v2 (English) vs v3 (25 languages) vs Moonshine on
recorded speech: latency and word errors. Both Parakeet exports are CC-BY-4.0
(attribution recorded in `models.toml` and an in-app licences list) from the
third-party repo `istupakov/parakeet-tdt-0.6b-v{2,3}-onnx`, pinned by commit and
hash. `transcribe.cpp` (new, MIT, same author as transcribe-rs) is a watch item.

## 6. TTS

### 6.1 Trait shape

`TextToSpeech` accepts sentences one at a time and owns its playback path. It
reports `FirstAudio`, `SentenceDone`, and `Idle` events and supports immediate
`stop()`. The trait deliberately does not return PCM, so an implementation may
either synthesize and play through `AudioOutput` (Kokoro) or hand text to an OS
speech engine (system voice). TTFA is taken from the `FirstAudio` event in both.

### 6.2 System voice: first implementation (M1)

`AVSpeechSynthesizer` through objc2 bindings. One utterance per sentence, queued
in order; cancel with `stopSpeakingAtBoundary(.immediate)`; the delegate's
did-start callback supplies `FirstAudio`. It needs no model download, no
phonemizer, and no extra permission. Voice and rate are configurable.

Purpose: sentence-level streaming works end to end from M1, and M2–M4 are not
blocked on Kokoro. It remains afterwards as the fallback when the Kokoro model is
not installed. The delegate callbacks need a running main-thread run loop, which
the app has from M2; the M1 CLI runs one for the duration of the reply.

### 6.3 Kokoro-82M v1.0 on `ort` (M1, second half)

- Source: `onnx-community/Kokoro-82M-v1.0-ONNX` (Apache-2.0). Benchmark
  `model.onnx` (326 MB fp32), `model_fp16.onnx` (163 MB), `model_q8f16.onnx`
  (86 MB) and keep the fastest one that sounds clean. Output is 24 kHz mono.
- Own thin wrapper (~300 lines) instead of an existing Kokoro crate: `kokoroxide`
  is on `ort` 1.16 and needs system espeak-ng; `kokoro-en` bundles espeak-ng by
  default. Inputs per the model card: `input_ids` (≤ 510 phoneme tokens), `style`
  (one 256-float row picked by token count), `speed`.
- Voices are raw float32 `.bin` files (~522 kB each), loaded under the `raw-f32`
  rule in §8.
- Kokoro runs faster than real time, so synthesis stays ahead of playback and
  sentences join without gaps. Escalation filler lines are pre-synthesized at
  startup when Kokoro is active.
- Phonemizer behind a trait: `misaki-rs` without espeak. Unknown words come back
  as "unknown", so glance adds a small fallback of its own (spell out / simple
  letter-to-sound).

**Time-boxed phonemizer spike.** misaki-rs 0.9.0 is very new, its docs.rs build is
failing, and it does not state compatibility with Kokoro's vocab. The spike is
limited to one working session and three checks: it builds with the `espeak`
feature off; every phoneme it emits over a fixed sentence set maps to a Kokoro
vocab ID; a listening pass over ~20 sentences is intelligible. If any check fails
or the time box runs out, work stops and the result is reported. Plan B (a
CMUdict-based G2P) is not started without the maintainer's go-ahead; the app keeps
using the system voice meanwhile.

### 6.4 ONNX Runtime provisioning

Needed from M0 (Parakeet) onward. By default `ort`'s build script downloads a
prebuilt ONNX Runtime from `cdn.pyke.io` and verifies it against SHA-256 values
shipped in the crate.

Preferred: `brew install onnxruntime`, with the build pointed at it via
`ORT_LIB_LOCATION` and `ORT_OFFLINE=1`, so the build fetches nothing. `xtask bundle`
copies the dylib into `Glance.app/Contents/Frameworks` and signs it.

Gate before relying on it: run `otool -L` on Homebrew's `libonnxruntime` dylib and
check for other Homebrew dependencies (anything under the Homebrew prefix besides
itself). If it has any, the bundle would have to carry and re-sign those too, so
fall back to `ort`'s checksum-verified download and tell the maintainer. The same
fallback applies if Homebrew's 1.32 does not work with `ort` rc.15 (which targets
the 1.31 API). CPU execution is expected to be enough for both Kokoro and Parakeet.

## 7. LLM tiers, endpoints, escalation

### 7.1 Endpoints

A tier is `{endpoint, model name, image size, ctx, extra_body}`. Endpoint kinds:
- `openai`: any OpenAI-compatible base URL (Ollama, LM Studio, user-configured).
- `managed-llama-server`: glance spawns `llama-server` for a manifest model:
  `--host 127.0.0.1 --port <random> -m … --mmproj … --offline --no-webui
  --parallel 1 --ctx-size N`, with a random per-launch API key passed through the
  child's environment (`LLAMA_API_KEY`) and held in memory only. One process per
  tier rather than llama-server's router mode: no LRU eviction surprises,
  independent context sizes, and a 1:1 mapping onto the config. At startup glance
  waits for `/health` and refuses a model whose `/props` reports no vision.

Hardware presets later are a named TOML overlay: config resolves as built-in
defaults → (future preset) → user file. Nothing else needs to change.

Models already installed in Ollama or LM Studio can join a bake-off through that
tool's endpoint at no download cost. Two things to know: Ollama unloads a model
after 5 minutes unless `OLLAMA_KEEP_ALIVE=-1` is set, and models managed by those
tools are outside glance's manifest.

### 7.2 Candidates (sizes read from Hugging Face)

| Tier | Model | Weights (4-bit) | mmproj | Licence |
|---|---|---|---|---|
| FAST | Qwen3-VL-4B-Instruct | 2.5 GB | 0.45–0.84 GB | Apache-2.0 |
| FAST | Qwen3-VL-8B-Instruct | 5.03 GB | 0.75–1.16 GB | Apache-2.0 |
| FAST | LFM2.5-VL-3B | 1.67 GB | 0.58–0.86 GB | `lfm1.0` (custom; LICENSE read before pinning) |
| DEEP | Qwen3.8-27B (bartowski) | 17.4 GB | 0.93 GB | Apache-2.0 |
| DEEP | Qwen3.6-35B-A3B (unsloth, MoE) | 22.1 GB | 0.90 GB | Apache-2.0 |
| DEEP | Gemma 4 31B-it (ggml-org, Q4_0) | 18 GB | 0.81–1.2 GB | Apache-2.0 |

Memory, estimated (weights + mmproj + ~1–2 GB context/overhead per model; STT ≈ 0.9,
TTS ≈ 0.4, app ≈ 0.3 GB):

| Pairing | Est. total | vs 30 GB budget |
|---|---|---|
| Qwen3-VL-4B + Qwen3.8-27B | ~26 GB | fits |
| Qwen3-VL-4B + Gemma 4 31B | ~27 GB | fits |
| Qwen3-VL-8B + Qwen3.8-27B | ~29 GB | tight |
| Qwen3-VL-4B + Qwen3.6-35B-A3B | ~30.5 GB | marginal |
| Qwen3-VL-8B + Qwen3.6-35B-A3B | ~33.5 GB | over |

So the MoE only fits beside a ~4B FAST model. Bake-off (FAST in M0, DEEP in M4): a
fixed local set of screenshots × questions, kept in a gitignored folder, measuring
cold and warm time-to-first-token, tokens/s, resident memory, escalation-format
compliance, and answer quality against a short rubric that the maintainer
spot-checks. The winner is reported with numbers, not guessed in advance.

### 7.3 Escalation schema

FAST either answers in plain speech, or outputs exactly one JSON object and nothing
else:

```json
{"glance":"escalate","v":1,"reason":"reasoning"}
```

```json
{ "type":"object", "additionalProperties":false,
  "required":["glance","v","reason"],
  "properties":{
    "glance":{"const":"escalate"},
    "v":{"const":1},
    "reason":{"enum":["reasoning","detail","code_math","uncertain","user_asked"]}}}
```

Parsing rules:
1. Look at the first non-whitespace character of FAST's stream. Anything but `{`
   means "answer": tokens flow straight to the sentence chunker. **The normal path
   pays zero extra latency.** A spoken answer never legitimately starts with `{`.
2. If it is `{`, buffer up to the closing `}` (hard cap 160 bytes), then parse with
   serde, `deny_unknown_fields`, exact constants, closed enum. Valid → cancel
   FAST's stream immediately and escalate.
3. Invalid → never escalates. The fragment is dropped (never spoken), a violation
   counter is logged, and glance speaks a fixed local line ("Sorry, ask me again").
4. At most one escalation per turn. DEEP's output is never parsed for control
   signals. If DEEP is unavailable or escalation is disabled, FAST's prompt simply
   omits the escalate option.
5. On llama-server, a GBNF grammar (`root ::= escalate | [^{] text`) makes rule 3
   unreachable. Other servers rely on prompt + parser. Whether the grammar is worth
   keeping is decided by the violation rate measured in M4.

On escalate: speak a filler line chosen from a fixed local list, then send DEEP
the original screenshot, history, and transcript. Nothing FAST wrote is forwarded.
`reason` is a closed enum used only for metrics and filler choice, so screen text
cannot smuggle words into anything.

Rejected: wrapping every reply in JSON (`response_format: json_schema`) or putting
a header line on every answer. Both add ~10 tokens before the first spoken word,
and the first needs a streaming JSON-string decoder. Tool-calling is rejected
because v1 has no tools and server support varies.

## 8. Model manifest, download, verification

`models.toml` is committed and compiled into the binary. A user-level manifest in
Application Support may add entries under the same rules.

```toml
schema = 1

[[model]]
id       = "qwen3-vl-4b-instruct-q4km"
role     = "vlm"                      # vlm | stt | tts
license  = "Apache-2.0"
repo     = "Qwen/Qwen3-VL-4B-Instruct-GGUF"
revision = "<40-hex commit>"          # never a branch name

  [[model.file]]
  path   = "Qwen3VL-4B-Instruct-Q4_K_M.gguf"
  kind   = "weights"                  # weights | mmproj | voice | aux
  format = "gguf"                     # gguf | onnx | safetensors | raw-f32 | text
  size   = 0                          # exact bytes
  sha256 = "<64 hex>"
```

Rules enforced by the parser, each with a test:
- The URL is built only as `https://huggingface.co/{repo}/resolve/{revision}/{path}`.
- `weights` and `mmproj` must be gguf, onnx, or safetensors. `.pt .pth .pkl .ckpt
  .npz .npy` are rejected outright. `.bin` is accepted only as `kind = "voice"`,
  `format = "raw-f32"`: read as little-endian floats, length must equal `size` and
  be a multiple of 256×4 bytes. No deserializer is involved.
- `aux` + `text` covers vocab, config, and tokenizer files (parsed as text or JSON
  only).
- Every file needs `size` and `sha256`; every model needs `license`.

Download (only on an explicit `glance models pull <id>` or a settings button,
never in the background):
1. HTTPS only, no cookies, no token, no proxy, generic `glance/<version>` user
   agent. Redirects are followed only to the exact allowlist: `huggingface.co`,
   `cdn-lfs.hf.co`, `cdn-lfs-us-1.hf.co`, `cdn-lfs-eu-1.hf.co`,
   `transfer.xethub.hf.co`, `transfer.xethub-eu.hf.co`, `cas-server.xethub.hf.co`,
   `cas-server.xethub-eu.hf.co`, and HF's published `*.aws.cdn.hf.co` and
   `*.gcp.cdn.hf.co` names, listed one by one. The `amazonaws.com` hosts in HF's
   own list are deliberately left out.
2. Stream into `models/.partial/`, hashing as bytes arrive; abort past `size`.
   Resume with Range, re-hashing the partial first.
3. On completion check size, SHA-256, and format magic, then atomically rename into
   `models/<id>/`. A mismatch deletes the partial and is a hard error.
4. Before use: small files are re-hashed on every load. Multi-GB files are
   re-hashed when size or mtime changed since the last verification, and on
   `glance models verify`. (Hashing 18 GB takes several seconds; doing it on every
   launch would hurt, and an attacker who can rewrite those files with user
   privileges can also replace the binary.)

Pinning: for each model, the SHA-256 that Hugging Face publishes and the one
computed locally after download are shown side by side; they must agree before the
entry is committed.

Boundary: the manifest governs what glance downloads and what it spawns. Models
managed by Ollama or LM Studio are that tool's responsibility.

## 9. UI: egui vs Tauri → egui

| | egui / eframe 0.37 | Tauri 2.12 |
|---|---|---|
| Stack | pure Rust, one process | Rust + webview + HTML/JS, IPC between them |
| Untrusted model output | drawn as plain glyphs | rendered in a webview: needs CSP and escaping discipline |
| Dependency surface | modest | large (plus usually npm) |
| Overlay | transparent, borderless, always-on-top, click-through, `with_active(false)` all exist | supported |
| Tray / hotkey | `tray-icon` + `global-hotkey` (same authors, same event loop) | built in |
| Looks | not native; fine for a status pill and a form | native-feeling settings |
| Bundling/signing | own xtask | built in |

Choice: **egui**. The UI is a status pill, a transcript, and a settings form;
model output derived from untrusted screen content never passes through an HTML
renderer; there is no second language or toolchain. Known gap: `with_active(false)`
only covers initial focus, so a truly non-activating overlay may need a small objc2
shim (window level / collection behavior). That is scoped to M5 and reviewed before
it is added. The overlay renders text only, with no clickable links.

## 10. Permission model

| Permission | Needed | Why | When prompted |
|---|---|---|---|
| Screen Recording | yes | ScreenCaptureKit screenshot | first capture (app relaunch may be required) |
| Microphone | yes | cpal input; `NSMicrophoneUsageDescription` + `com.apple.security.device.audio-input` | first key-down |
| Input Monitoring | **no** | Carbon hotkeys do not observe keystrokes | — |
| Accessibility | no | no input synthesis, no UI scripting | — |
| Keychain | only with a remote endpoint | API key storage | when a key is saved |

The mic stream exists only while the key is held, so the system's microphone
indicator matches exactly. Bare-modifier hotkeys (which do need Input Monitoring)
can be added later as a second `HotkeySource` implementation.

**Stable identity so grants survive rebuilds.** macOS ties permission grants to
bundle ID plus signing identity; ad-hoc signatures change on every build.
`cargo xtask bundle` assembles `Glance.app` at a fixed path with:
- bundle ID `io.github.<username>.glance` (the GitHub username is filled in once,
  before M2);
- `LSUIElement`, usage strings, hardened runtime, entitlements;
- a signature, on the app and the bundled ONNX Runtime dylib, from a stable
  code-signing certificate. The identity name is read from the
  `GLANCE_SIGN_IDENTITY` environment variable and never committed.

M2's acceptance test: grant once, rebuild twice, no new prompts.

M0 and M1 are CLI spikes run from a terminal, so the mic prompt is attributed to
the terminal app, and no screen capture happens (M0 reads a PNG supplied on the
command line).

The tray menu gets a "Permissions…" checklist showing each state with a deep link
to the right System Settings pane.

## 11. Security rules → mechanisms

| Rule (CLAUDE.md) | Mechanism |
|---|---|
| Screenshots/audio in memory only | opaque types (§2); no temp files; `--debug-dump` off by default, prints a warning and shows a tray marker |
| No telemetry or pings | no HTTP client outside `models` and `llm-client`; downloads only on explicit action |
| Loopback by default | endpoint host must resolve to a loopback address, checked at connect time; redirects disabled; proxies ignored. `allow_remote = true` per endpoint turns on a persistent "screen is being sent to <host>" marker in tray and overlay |
| Spawned llama-server | `127.0.0.1`, random port, `--offline`, per-launch API key |
| API keys | Keychain only; config holds a reference, never the key |
| Prompt injection | no tools, no actions; output is displayed as plain text and spoken; escalate parsed per §7.3; one escalation per turn |
| Weights | §8 |
| Logs | default logs carry timings and sizes only, never transcript or reply text; `--log-content` is an explicit dev opt-in |
| Supply chain | exact version pins, `Cargo.lock` committed, `cargo audit` + `cargo deny` (advisories, licence allowlist with GPL denied, crates.io-only sources), gitleaks pre-commit hook and CI job |

## 12. Latency budget

Targets on the reference machine (Apple Silicon M4 Pro, 48 GB) for a 5 s question.
These are estimates, replaced by measurements from M0 onward. One structured log
line per turn carries every field, plus llama-server's own `timings` (`prompt_n`,
`cache_n`, `predicted_per_second`).

| Stage | Logged as | Target | Notes |
|---|---|---|---|
| Capture | `capture_ms` | ≤ 80 ms | at key-down, off the critical path |
| Downscale + PNG | `encode_ms` | ≤ 60 ms | at key-down |
| FAST image prewarm | `prewarm_ms` | 1.5–3 s | hidden behind the user's speech |
| Audio finalize + resample | `audio_ms` | ≤ 30 ms | after key-up |
| STT | `stt_ms` | ≤ 300 ms | Parakeet int8 |
| FAST time-to-first-token | `ttft_ms` | ≤ 350 ms warm | cold adds the prewarm cost |
| First speakable chunk | `first_chunk_ms` | ≤ 350 ms | ~12–15 tokens |
| TTS first chunk | `tts_first_ms` | ≤ 250 ms | Kokoro target; system voice measured in M1 |
| Output start | — | ≤ 30 ms | |
| **Time-to-first-audio** | `ttfa_ms` | **≤ 1.3 s** (stretch 0.9 s) | key-up → first audio; cold ≤ 4 s |

Escalated turn: filler audio ≤ 1.1 s after key-up; DEEP first audio ≤ 4 s after
the filler when prewarmed. Cold DEEP (image not yet processed) could be 5–12 s,
which is why DEEP prewarm exists.

Mic start-up may clip the first syllable (CoreAudio stream start). Measured in M0;
the mitigation is building the stream at launch and only starting it on key-down,
provided the microphone indicator stays off while it is paused.

## 13. Not yet verified, and where it gets settled

| Open item | Settled in |
|---|---|
| llama-server reuses a cached image prefix across requests (prewarm relies on it) | M0, first task (`timings.cache_n`) |
| Homebrew `libonnxruntime` has no other Homebrew dependencies (`otool -L`) and works with `ort` rc.15 | M0 STT spike |
| Parakeet meets the STT budget | M0 STT spike |
| Chat endpoint accepts `grammar` with `stream: true`; streamed `reasoning_content` field | M0 / M4 |
| AVSpeechSynthesizer via objc2 bindings: delegate callbacks, run loop in a CLI | M1 |
| misaki-rs phonemes match Kokoro's vocab; quality of the no-espeak fallback | M1 time-boxed spike |
| A chord claimed by a macOS system shortcut may register without error yet never fire | M2 |
| Permission grants persist across rebuilds with a stable signing identity | M2 |
| ScreenCaptureKit crate's exact filter API for excluding glance's own windows | M2 |
| Vision support of models served through Ollama/LM Studio endpoints | M4 |
| egui overlay stays non-activating without a shim | M5 |

## 14. Milestones

- **Step 2, skeleton**: `.gitignore` and gitleaks hook first, CLAUDE.md amendments,
  workspace, `deny.toml`, CI file. Run: `cargo test`, `cargo deny check`.
- **M0, CLI spike**, in this order:
  1. Verify image-prefix reuse: send a real screenshot (a PNG supplied on the
     command line) to llama-server twice, image-only then image + question, and
     confirm from `timings.cache_n` that the second request reuses the image
     tokens. If it does not, stop and report before going further, because §4's
     prewarm design depends on it.
  2. STT spike (includes the ONNX Runtime gate in §6.4).
  3. FAST bake-off report.
  Deliverable: `glance spike --png shot.png` records 5 s, transcribes, asks
  llama-server, prints the reply and per-stage times.
- **M1, speech out**: `TextToSpeech` + `SentenceChunker` with the system-voice
  implementation; the reply is spoken sentence by sentence and TTFA is reported.
  Then the time-boxed phonemizer spike (§6.3) and, if it passes, the Kokoro
  implementation.
- **M2**: `Glance.app` with hotkeys, live capture, tray status, permission
  checklist. Acceptance includes: both chords register at startup, a registration
  failure produces a visible error naming the chord, a physical press and release
  of each chord is observed end to end, and grants survive two rebuilds.
- **M3**: follow-ups on the same screenshot, `look` hotkey, session expiry.
- **M4**: escalation, managed llama-server pair, DEEP bake-off report.
- **M5**: settings, overlay, Keychain, model manifest management.

Each milestone ends with tests, `cargo audit`, `cargo deny check`, a commit (never
a push), a summary under 10 lines, and a stop for the maintainer's go-ahead.
