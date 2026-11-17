# glance — progress

Snapshot as of 2026-11-17. The design lives in `docs/DESIGN.md`; this file says
where the work stands. All numbers were measured on the Apple Silicon
reference machine named in `CLAUDE.md`, with synthetic inputs unless noted.

## Milestone status

| Step | Status |
|---|---|
| Step 1: design (`docs/DESIGN.md`) | Done, approved |
| Step 2: skeleton, hooks, CI, `deny.toml` | Done. CI has not been observed running on GitHub yet |
| M0: CLI spike (record, transcribe, ask, print, time) | Done, except a spoken run with a real voice (needs the maintainer) |
| M1: TTS with sentence streaming | **Half done.** System voice works end to end; Kokoro passed its spike but is not built into the app |
| M2–M5 | Not started |

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

**End to end (M0).** Key-up to first complete sentence: 404 ms. Mic delivers
first samples 65 ms after start; 48→16 kHz resample 1–2 ms.

**Time to first audio, system voice (M1).** Key-up to first sound, six runs
of `glance spike` with synthetic clips: 354–546 ms (target 1.3 s), including
runs where the image was not yet cached. The voice starts 36–60 ms after a
sentence is queued. All runs were at volume 0; nobody has listened yet.

**Kokoro spike (M1, in `spikes/kokoro-g2p`).**
- `misaki-rs` 0.9.0 builds with no espeak. G2P load 25–64 ms, ~2.7 ms per
  sentence.
- Every phoneme it emitted over 60 assistant-style sentences is in Kokoro's
  115-symbol vocabulary, but 15 words had no pronunciation (clock times,
  names, file paths, "Gmail", "Kubernetes"). With the spike's own fallback,
  none were left.
- Synthesis per sentence (median): fp32 at 10 threads 456 ms (9.1x real
  time); fp32 at 4 threads 728 ms; q8f16 1.4 s (2.8x). fp32 is the one to use.
- Intelligibility by proxy: transcribing Kokoro's output with Parakeet gave
  9.9% word errors vs 9.5% for the macOS voice on the same 60 sentences.
  Errors are mostly number formatting and unusual names.

## Decisions made, and why

- **`screencapturekit` crate, not `xcap`** for capture: xcap uses the legacy
  `CGWindowListCreateImage` path.
- **STT is Parakeet v3** (v2 pinned as English-only alternative): far inside
  budget, ONNX is an allowed format, shares ONNX Runtime with Kokoro.
  Whisper's GGML `.bin` is banned; Moonshine is hosted off the allowlist.
- **ONNX Runtime comes from `ort`'s build-time download** (cdn.pyke.io,
  SHA-256 checked, statically linked). Homebrew's build failed the gate: it
  links onnx, protobuf, re2 and ~80 abseil dylibs. `deny.toml` allows `ureq`,
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
- **Commit per crate or logical unit**, hooks always on, never push
  (`CLAUDE.md`).

## Open items and known problems

1. **`cargo deny check` fails on advisories since 2026-11-17.**
   RUSTSEC-2026-0542 marks `humantime` as unmaintained (not a
   vulnerability); it comes in via `transcribe-rs` → `env_logger` 0.10, and
   no fixed version exists. Options: add an `ignore` entry with a reason to
   `deny.toml`, or replace or patch the dependency. Needs a decision; CI fails
   until then. `cargo audit` reports it as a warning only.
2. **Kokoro or not.** Kokoro adds ~250–450 ms per first sentence compared
   with the system voice (~35 ms). Undecided whether to build it now or park
   it while M2 uses the system voice.
3. Nobody has listened to either voice, or run `glance spike` with a real
   voice in a real room.
4. Only default-quality system voices are installed on the reference machine;
   a Premium voice should sound clearly better.
5. Kokoro still mispronounces some names ("Tomas", "Okafor") and compounds
   ("Harbor-5G", "CrashLoopBackOff").
6. The system-voice code has no automated test; `cargo test` runs no main
   run loop. The chunker and text cleanup are covered (15 tests).
7. Both Qwen models hit the ceiling of the synthetic set, so it cannot show
   where the 8B reads better than the 4B.
8. `docs/DESIGN.md` §6 and §13 do not yet include the M1 numbers above.

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
glance spike ... --wav clip.wav --volume 0   # scripted, silent run
glance cache-check --png shot.png --endpoint ... --model ...
glance say "Text to speak."                  # system voice only
glance voices                                # installed English voices
glance mic-check                             # level and timing, no transcript
glance stt-bench --stt parakeet-tdt-0.6b-v3-int8 clips/*.wav
glance bench --endpoint ... --model ... --out eval-local/results/x.json
```

Rendering the evaluation screens and running the Kokoro spike: see `README.md`
and `spikes/README.md`.

Checks (run before calling a milestone done):

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked              # 60 tests
cargo audit
cargo deny check                             # currently fails, see open item 1
```

## Next step

Decide open items 1 and 2. Then finish M1:
1. If Kokoro goes ahead: a cpal `AudioOutput` in `crates/audio`, the
   phonemizer and fallback moved into `crates/tts` with tests, a Kokoro
   `TextToSpeech` (fp32, 10 threads, synthesis running ahead of playback),
   and a flag to choose the engine.
2. Record the M1 numbers in `docs/DESIGN.md`, run the checks, commit, and
   write the milestone summary.

M2 (hotkey, live capture, `.app` bundle, permissions) follows. It needs the
GitHub username for the bundle ID `io.github.<username>.glance`.
