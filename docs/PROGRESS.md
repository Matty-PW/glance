# glance — progress

Snapshot as of 2026-11-17; the design is in `docs/DESIGN.md`. Numbers are from
the Apple Silicon reference machine, with synthetic inputs unless noted.

## Milestone status

| Step | Status |
|---|---|
| Step 1: design (`docs/DESIGN.md`) | Done, approved |
| Step 2: skeleton, hooks, CI, `deny.toml` | Done. CI has not been observed running on GitHub yet |
| M0: CLI spike (record, transcribe, ask, print, time) | Done, except a spoken run with a real voice (needs the maintainer) |
| M1: TTS with sentence streaming | Done on the macOS system voice |
| Kokoro voice (resumed after M3) | **Code done; Kokoro is the default voice.** Licence decision and listening test left (see below) |
| M2: hotkey, live capture, `.app`, permissions | **Code done; acceptance needs the maintainer** (see "Next step") |
| M3: follow-ups, `look` vs `talk`, session expiry | **Code done; needs a spoken run through the app** |
| M4–M5 | Not started |

## Kokoro milestone (resumed 2026-11-17)

Kokoro-82M is the default voice; the system voice stays available
(`engine = "system"` in the config, or `--engine system`).

Done: phonemizer (`misaki-rs`, espeak off, plus glance's fallback for unknown
words, behind the `Phonemizer` trait); voice tables (exact length and NaN
check) and weighted blends; the ONNX wrapper (splits over 510 tokens, rejects
non-finite output); `AudioOutput` + cpal `Speaker`; `KokoroVoice` behind
`TextToSpeech` (synthesizes ahead, immediate stop, device opened at key-down
via `warm_up` and closed after 30 s idle); config file; `glance voices`,
`glance say --voice/--speed`, `glance tts-bench`; all 28 English voices pinned.

Left:
1. **Licence decision: `language-tokenizer` 0.1.0 (WTFPL)**, a dependency of
   misaki-rs that misaki-rs never uses. `cargo deny check` fails on licences
   until it is allowed (scoped exception) or removed.
2. Listening test of the samples (`eval-local/kokoro-bench/`) and of voices.
3. A spoken run through `glance app` with Kokoro, recording the turn's
   `ttfa_ms`.
4. Pre-synthesize the escalation filler lines at startup (with M4).
5. Possibly shorten the first chunk for Kokoro (its synthesis time grows with
   length; the chunker's early first chunk is 40 characters).

Phonemizer decision: `THIRD_PARTY.md` and `docs/DESIGN.md` §6.3.

## What is measured

**Image-prefix cache (M0).** On a real screenshot with Qwen3-VL-4B, the
image-only prewarm evaluated 665 tokens in 1.8 s; the question that followed
reused 660 and produced its first token in 214 ms; a follow-up reused 718.
LFM2.5-VL-3B reused nothing for the first question.

**Speech-to-text (M0).** Parakeet TDT 0.6b int8 on CPU: ~35x real time
(~90 ms for a 3 s clip), 0.5 s load. Word errors on 16 synthetic clips
(158 words): v2 0/158, v3 2/158 (both British spellings).

**FAST bake-off (M0).** Six synthetic screens, 30 read-the-screen questions,
6 that should escalate:

| Model | Edge | Correct | New image | Speed | Answered directly | Escalated | Malformed | Memory |
|---|---|---|---|---|---|---|---|---|
| **Qwen3-VL-4B** | **1024** | 30/30 | 1.5 s | 72 tok/s | 29/30 | 6/6 | 0 | 4.5 GB |
| Qwen3-VL-8B | 1024 | 30/30 | 2.5 s | 44 tok/s | 19/30 | 6/6 | 0 | 6.3 GB |
| LFM2.5-VL-3B | 1280 | 29/30 | 3.1 s | 107 tok/s | 23/30 | 1/6 | 12 | 1.6 GB |

**Live capture (M2).** `glance capture-check` on the built-in display:
capture at 2048 px 46–49 ms warm, 112 ms for the first; at full size
(3024x1955) 53–169 ms; downscale + PNG to 1024 px 14 ms. Capture runs at
key-down, alongside the user's speech, so it is off the TTFA path.

**Kokoro vs the system voice (`glance tts-bench`).** Eight first-chunk
sentences (5–70 characters), voice af_heart, ONNX Runtime CPU.

| Model | Load | Synth median, 4 / 8 / 10 threads | TTFA cold | TTFA warm median / max |
|---|---|---|---|---|
| fp32 (326 MB) | 0.25 s | 629 / 426 / 392 ms | 391 ms | 437 / 565 ms |
| fp16 (163 MB) | – | fails: every sample NaN or infinite on the CPU path | – | – |
| q8f16 (86 MB) | 0.47 s | 1535 / 1474 / 1352 ms | 1004 ms | 1344 / 1888 ms |
| system voice | – | – | 34 ms | 1 / 1 ms* |

TTFA here is `speak()` until the first sample reaches the device (Kokoro) or
the system voice's start callback (*which can come before sound). fp32 runs
8.8x real time at 10 threads, so later sentences are ready before the first
ends. Choice: **fp32, 10 threads**. Expected app TTFA: key-up to first chunk
~0.4 s (M0/M1) + ~0.44 s = ~0.85 s, under the 1.3 s target.

**Time to first audio, system voice (M1).** Key-up to first sound over six
`glance spike` runs: 354–546 ms (target 1.3 s), including uncached images
(M0: 404 ms to first complete sentence). All runs at volume 0.

## Decisions made, and why

- **`screencapturekit` crate, not `xcap`** for capture: xcap uses the legacy
  `CGWindowListCreateImage` path.
- **STT is Parakeet v3** (v2 pinned as alternative): far inside budget, ONNX
  format. Whisper's GGML `.bin` is banned; Moonshine is hosted off-allowlist.
- **ONNX Runtime from `ort`'s build-time download** (SHA-256 checked, static).
  Homebrew's build links ~80 other Homebrew dylibs. `deny.toml` allows `ureq`,
  `native-tls` and one data licence only beneath `ort-sys`'s build script.
- **`ort` held at 2.0.0-rc.12**: `transcribe-rs` 0.3.12 does not compile
  against later release candidates.
- **FAST = Qwen3-VL-4B at 1024 px** (table above). 1024 px loses nothing on
  this set; 896 px missed 2 questions.
- **System voice first** (`AVSpeechSynthesizer`). macOS delivers its
  callbacks only on the main thread's run loop, so the CLI keeps the main
  thread pumping that loop and runs the pipeline on tokio worker threads.
- **No GPL code**: `misaki-rs` with `default-features = false`, plus an
  in-house fallback for unknown words instead of espeak-ng.
- **M2 turn loop** (`crates/orchestrator`): the microphone and STT each run
  on their own OS thread fed by a channel, so a stop never overtakes its
  start. A cancelled turn's task is aborted (dropping the chat stream cancels
  it on the server); its leftover speech events are ignored by utterance id.
  A failed turn speaks a fixed local line ("Sorry, I didn't catch that").
- **Permissions**: Screen Recording is requested at launch (the prompt shows
  once; a grant needs a relaunch). Microphone status is read only; macOS
  prompts on the first stream open, so the very first press records silence
  and glance says to check the permission.
- **Tray** shows state as a short marker next to the icon (● listening,
  … working, ▸ speaking, ! fatal) because there is no overlay until M5.
- **M3 sessions** (`crates/orchestrator/src/session.rs`): only answered
  turns join the session, so a cancelled or failed `look` keeps the old
  screenshot. `talk` follow-ups skip capture and prewarm (the prefix is
  cached from the last turn). On `look`, the older image becomes a short
  text placeholder and the new image goes in the new question, so one image
  is ever sent and the prewarm is history + new image. History is capped at
  12 exchanges (well inside `glance serve`'s 8192-token context); when the
  oldest is dropped the screenshot moves to the next question. "New
  session" in the tray and 10 idle minutes end it.
- **Signing** uses `--timestamp=none`: a secure timestamp contacts Apple and
  is only needed for notarization.
- **Builds target macOS 14.2**, and `crates/capture/build.rs` lets the
  ScreenCaptureKit Swift bridge link with only the Command Line Tools.

## Open items and known problems

1. **RUSTSEC-2026-0542 is ignored in `deny.toml`** (maintainer's decision,
   2026-11-17). `humantime` is flagged unmaintained only (no vulnerability,
   no fix); it comes via `transcribe-rs` 0.3.12 → `env_logger` 0.10, which
   transcribe-rs uses only in examples and tests. glance never starts a
   logger, and neither crate's code is in the release binary (`nm`). Remove
   the ignore once transcribe-rs drops env_logger 0.10 (env_logger 0.11 uses
   `jiff` instead) or glance drops transcribe-rs.
2. Nobody has listened to either voice or run `glance spike` with a real voice.
3. Only default-quality system voices are installed; a Premium one is better.
4. The system-voice, hotkey and tray code have no automated tests (they need
   a main run loop). The orchestrator is covered with mocks (15 tests).
5. Both Qwen models hit the ceiling of the synthetic set, so it cannot show
   where the 8B reads better than the 4B.
6. Not yet observed: a physical press of either chord, a turn through the
   bundled app, and grants surviving rebuilds (all need the maintainer).
   Observed: a registration failure names both chords (a second instance
   started while the bundle was running).
7. Not yet measured: TTFA of a `talk` follow-up (no capture, no prewarm)
   and of a `look` with history, where the prewarm re-evaluates the
   history text before the new image.
8. `glance spike` keeps its own copy of the pipeline (it reads a PNG and
   prints a timing table); the app uses the orchestrator.

## How to run what exists

```sh
brew install llama.cpp gitleaks cargo-audit cargo-deny
git config core.hooksPath .githooks          # once per clone
alias glance='cargo run --release -q -p glance --'

glance models pull qwen3-vl-4b-instruct-q4km # FAST model, 3.3 GB
glance models pull parakeet-tdt-0.6b-v3-int8 # speech-to-text, 0.7 GB
glance models list | verify                  # status / re-hash everything

glance serve qwen3-vl-4b-instruct-q4km       # prints http://127.0.0.1:<port>/v1
# in another terminal:
glance spike --png shot.png --endpoint http://127.0.0.1:<port>/v1 \
  --model qwen3-vl-4b-instruct-q4km          # record 5 s, answer aloud, timings
glance cache-check --png shot.png --endpoint ... --model ...
glance voices                                # Kokoro and system voices
glance say --voice bf_emma --speed 1.1 "Text to speak."
glance say --voice af_heart:0.6,bm_george:0.4 "A blend."
glance say --engine system --voice en-GB "The system voice."
glance tts-bench --out eval-local/kokoro-bench # Kokoro models vs system voice
glance capture-check                         # live capture: sizes and timings only
export GLANCE_SIGN_IDENTITY=<SHA-1 from: security find-identity -v -p codesigning>
cargo xtask bundle                           # signed target/bundle/Glance.app
open -W --stderr "$(tty)" target/bundle/Glance.app --args app \
  --endpoint http://127.0.0.1:<port>/v1 --model qwen3-vl-4b-instruct-q4km
glance mic-check                             # level and timing, no transcript
glance stt-bench --stt parakeet-tdt-0.6b-v3-int8 clips/*.wav
glance bench --endpoint ... --model ... --out eval-local/results/x.json

# checks, before calling a milestone done
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked              # 109 tests
cargo audit
cargo deny check                             # fails on licences: Kokoro item 1
```
Eval screens and Kokoro spike: `README.md`, `spikes/README.md`.

## Next step: M2 and M3 acceptance (needs the maintainer), then M4

Built: the orchestrator turn loop and sessions (`crates/orchestrator`),
`glance app` (`crates/app/src/cmd/app/`), permission helpers, `cargo xtask
bundle`, `glance capture-check`. Live capture has run on a real screen from
the terminal. To finish M2 and M3:
1. Quit any running Glance (a rebuild replaces the bundle under it). Set
   `GLANCE_SIGN_IDENTITY`, run `cargo xtask bundle` (click "Always Allow" if
   the keychain asks), start `glance serve`, launch with the `open` command
   above.
2. Allow Screen Recording when asked, quit from the tray, relaunch.
3. Hold Ctrl+Opt+G and ask; allow the microphone prompt; ask again. Each
   turn logs one `turn=… ttfa_ms=…` line.
4. M3: ask a follow-up with Ctrl+Opt+G (log shows `image=reused`), change
   the screen and ask with Ctrl+Opt+Shift+G (`image=fresh`), then a
   follow-up about the new screen. Check the tray's session line and "New
   session".
5. Rebuild twice (`cargo xtask bundle`), relaunch, and check no new prompts
   appear. Then M4 (escalation, managed llama-server pair, DEEP bake-off).
