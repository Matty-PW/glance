# glance — progress

Snapshot as of 2026-11-17; the design is in `docs/DESIGN.md`. Numbers are from
the Apple Silicon reference machine, with synthetic inputs unless noted.

## Milestone status

| Step | Status |
|---|---|
| Step 1: design (`docs/DESIGN.md`) | Done, approved |
| Step 2: skeleton, hooks, CI, `deny.toml` | Done. CI has not been observed running on GitHub yet |
| M0: CLI spike (record, transcribe, ask, print, time) | Done, except a spoken run with a real voice (needs the maintainer) |
| M1: TTS with sentence streaming | Done on the macOS system voice. **Kokoro parked** (see below) |
| M2: hotkey, live capture, `.app`, permissions | **Code done; acceptance needs the maintainer** (see "Next step") |
| M3–M5 | Not started |

## Decision: Kokoro parked (2026-11-17)

M2–M4 use the macOS system voice so the full loop works end to end first.
Kokoro is revisited after M4, or sooner if system-voice quality turns out to
be a real problem. Reason: Kokoro would add roughly 250–450 ms before the
first sentence sounds. Measured synthesis per sentence (fp32, 10 threads):
230–804 ms, median 456 ms; the system voice starts 36–60 ms after queueing.
Spike (`spikes/kokoro-g2p`): `misaki-rs` without espeak covers Kokoro's
vocabulary once the in-house fallback handles unknown words; transcribed back,
Kokoro scored 9.9% word errors vs 9.5% for the system voice.

Left to finish Kokoro:
1. `AudioOutput` trait and cpal implementation in `crates/audio`: 24 kHz to
   device-rate resampling, ring buffer, immediate stop, start/finish events.
2. Move `spikes/kokoro-g2p/src/fallback.rs` into `crates/tts` as the
   `Phonemizer` over `misaki-rs` (`default-features = false`, exact pin), with
   unit tests. Fix the known misreads: names, mixed tokens like "Harbor-5G",
   CamelCase like "CrashLoopBackOff".
3. A Kokoro `TextToSpeech`: load the verified `kokoro-82m-v1.0-fp32`, map
   phonemes via `tokenizer.json`, read `raw-f32` voices with a length check,
   pick the style row by token count, split input over 510 tokens, synthesize
   ahead of playback on its own thread, emit events, flush on `stop()`.
4. Pre-synthesize the escalation filler lines at startup.
5. Engine choice in settings, falling back to the system voice when the
   model is not installed.
6. Measure TTFA with Kokoro vs the system voice and do a listening test.
7. Licence check: `misaki-rs` is MIT, but its README defers to upstream
   Misaki for the licence of its embedded dictionary data.

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
- **Signing** uses `--timestamp=none`: a secure timestamp contacts Apple and
  is only needed for notarization.
- **Builds target macOS 14.2**, and `crates/capture/build.rs` lets the
  ScreenCaptureKit Swift bridge link with only the Command Line Tools.

## Open items and known problems

1. **`cargo deny check` fails on advisories since 2026-11-17** (CI too):
   RUSTSEC-2026-0542, `humantime` unmaintained (not a vulnerability), via
   `transcribe-rs` → `env_logger` 0.10, no fixed version. Decide: `ignore`
   entry with a reason in `deny.toml`, or replace/patch the dependency.
2. Nobody has listened to either voice or run `glance spike` with a real voice.
3. Only default-quality system voices are installed; a Premium one is better.
4. The system-voice, hotkey and tray code have no automated tests (they need
   a main run loop). The orchestrator is covered with mocks (15 tests).
5. Both Qwen models hit the ceiling of the synthetic set, so it cannot show
   where the 8B reads better than the 4B.
6. Not yet observed: a physical press of either chord, a turn through the
   bundled app, and grants surviving rebuilds (all need the maintainer).
7. `glance spike` keeps its own copy of the pipeline (it reads a PNG and
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
glance say "Text to speak." ; glance voices  # system voice, installed voices
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
cargo test --workspace --locked              # 80 tests
cargo audit
cargo deny check                             # currently fails, see open item 1
```
Eval screens and Kokoro spike: `README.md`, `spikes/README.md`.

## Next step: M2 acceptance (needs the maintainer)

Built: the orchestrator turn loop (`crates/orchestrator`), `glance app`
(`crates/app/src/cmd/app/`), permission helpers, `cargo xtask bundle`,
`glance capture-check`. Live capture has run on a real screen from the
terminal. To finish M2:
1. Decide open item 1 (`cargo deny` advisory).
2. Set `GLANCE_SIGN_IDENTITY`, run `cargo xtask bundle` (click "Always
   Allow" if the keychain asks), start `glance serve`, launch the bundle with
   the `open` command above.
3. Allow Screen Recording when asked, quit from the tray, relaunch.
4. Hold Ctrl+Opt+G and ask; allow the microphone prompt; ask again. Repeat
   with Ctrl+Opt+Shift+G. Each turn logs one `turn=… ttfa_ms=…` line.
5. Rebuild twice (`cargo xtask bundle`), relaunch, and check no new prompts
   appear. Then M3 (follow-ups, `look` vs `talk`, session expiry).
