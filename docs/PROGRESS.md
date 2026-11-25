# glance — progress

Snapshot as of 2026-11-25; the design is in `docs/DESIGN.md`. Numbers are from
the Apple Silicon reference machine, with synthetic inputs unless noted.

## Milestone status

| Step | Status |
|---|---|
| Step 1: design (`docs/DESIGN.md`) | Done, approved |
| Step 2: skeleton, hooks, CI, `deny.toml` | Done. CI has not been observed running on GitHub yet |
| M0: CLI spike (record, transcribe, ask, print, time) | Done, except a spoken run with a real voice (needs the maintainer) |
| M1: TTS with sentence streaming | Done (Kokoro default, macOS voice as an option) |
| Kokoro voice (resumed after M3) | **Code done; Kokoro is the default voice.** Licence decision and listening test left (see below) |
| M2: hotkey, live capture, `.app`, permissions | **Code done; acceptance needs the maintainer** (see "Next step") |
| M3: follow-ups, `look` vs `talk`, session expiry | **Code done; needs a spoken run through the app** |
| M4: escalation, managed llama-server pair, DEEP bake-off | **Code done.** DEEP chosen (Qwen3.8-27B, pinned) and measured; FAST-only default, think harder on demand |
| M5: overlay, settings, model management | **Code done.** Overlay, settings window, models page, personality, visual redesign, screenshot indicator; hands-on checks next |
| Public release preparation | **Ready, not public.** Licence chosen, log gaps fixed; the rest is the maintainer's checklist (see below) |

## Public release preparation (2026-11-22)

Prepared only: the repository's visibility, history and licence are the
maintainer's decisions.
- Audit (full report in the gitignored `eval-local/public-audit.md`):
  gitleaks over all history clean; no home paths, tokens or keys; no binary
  or large files ever committed (largest blob `Cargo.lock`, 83 KB; repo
  about 0.6 MB packed). Commit author email kept as is (maintainer,
  2026-11-22: accepted).
- RUSTSEC-2026-0592 (`url`, published 2026-11-20) made `cargo audit` and
  `cargo deny` fail: fixed by `url` 2.5.9 (lockfile only).
- CI reviewed: one action pinned to a full SHA, read-only token, no
  `pull_request_target`, no secrets or deploy steps; fmt, clippy, tests,
  audit, deny and gitleaks all run. No change needed.
- Logs: per-turn lines hold codes and timings only. Two gaps found and
  fixed (2026-11-22): a Kokoro phonemizer error could name a word of an
  answer (now the error keeps only the sentence length and the log line a
  code and length), and `ChatError` kept the server's error body and
  serde's message (now a status code plus a fixed list of known messages,
  and fixed protocol strings; the app's turn line already showed only the
  status). Tests check neither path can carry answer or server text.
- New files: README rewrite, SECURITY.md, CONTRIBUTING.md, issue and pull
  request templates, `docs/images/` (PNGs allowed only there), `*.log`
  ignored. THIRD_PARTY.md now lists every model and licence, the Parakeet
  CC-BY-4.0 attribution and the LFM Open License terms in plain words.
- Decisions (maintainer, 2026-11-22): licence **MIT OR Apache-2.0**
  (`LICENSE-MIT`, `LICENSE-APACHE`, every crate's `license`); misaki
  dictionary data **kept with the note, source only**: no binaries are
  distributed, and the README says this must be resolved (likely CMUdict)
  before any are; security reports through GitHub private vulnerability
  reporting only (no email); the eval mock screen now uses 192.0.2.x.
- Left for the maintainer: repo URL in the README, acknowledgement time in
  SECURITY.md, description/topics/social preview, private vulnerability
  reporting and branch protection on GitHub, logo and demo, name check,
  first CI run, and making the repository public.

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
1. Done: `language-tokenizer@0.1.0` (WTFPL, unused by misaki-rs) allowed by
   an exact-version exception in `deny.toml`; review in `THIRD_PARTY.md`.
   Remove it once misaki-rs drops the dependency (upstream issue drafted,
   not filed).
2. Listening test of the samples (`eval-local/kokoro-bench/`) and of voices.
3. A spoken run through `glance app` with Kokoro, recording the turn's
   `ttfa_ms`.
4. Pre-synthesize the escalation filler lines at startup (with M4).
5. Possibly shorten the first chunk for Kokoro (its synthesis time grows with
   length; the chunker's early first chunk is 40 characters).

Phonemizer decision: `THIRD_PARTY.md` and `docs/DESIGN.md` §6.3.

## M5 (in progress)

Decisions (maintainer, 2026-11-18): native macOS UI (non-activating NSPanel
overlay, native settings window), small and behind a trait; settings cover
voice, length, models, think-harder options and hotkeys, config.toml stays
the source of truth for the rest; voice, length and think-harder apply live,
hotkeys live only if the new chord registers, model changes via "Apply and
restart". Remote endpoints and the Keychain are a later milestone with a
security review. All macOS code lives in `crates/macos` (`glance-macos`);
no AppKit type reaches the core crates; no Windows code yet.

Done:
- Platform split: ScreenCaptureKit capture, permissions, system voice,
  Carbon hotkeys, the app shell and the bundler moved into `glance-macos`;
  the app talks to the UI through the orchestrator's `Shell` trait.
- Overlay (`crates/macos/src/overlay.rs`): borderless non-activating
  NSPanel, click-through, every Space, top right; status line (state,
  think-harder stage, DEEP loading, screenshot age), the question, and the
  answer sentence by sentence as it sounds; hides 6 s after the turn.
  Text arrives on a display-only feed (`Shown`), never logged.
  `[overlay] enabled` in config.toml; `glance app --overlay-demo` plays a
  scripted turn to check its look.

- Stale llama-server cleanup: PID files in
  `~/Library/Application Support/glance/run/`; at start-up a server whose
  glance is gone is stopped only if its command line is still
  `llama-server --host 127.0.0.1 --port <that port>`. Tested with fakes
  (6 tests) and for real: a SIGKILLed glance's orphan was stopped by the
  next start.
- Settings window (`crates/macos/src/settings.rs`, tray "Settings…" or
  `glance app --open-settings`): voice engine, voice, Kokoro speed, a
  three-stop length slider, DEEP reasoning effort and idle unload (all
  live), FAST/DEEP model, DEEP thinking cap and auto-escalation ("Apply and
  restart"), and the three hotkeys (live; each new chord registers before
  the old one is released, and a failure keeps the old chords and says
  why). Portable types and the `SettingsBackend` trait are in
  `glance_orchestrator::settings`; the app implements it. Changes are
  written into config.toml in place (`crates/app/src/config_edit.rs`:
  other lines and comments kept, the result is validated with the strict
  parser before it replaces the file). Switching voice engine needs a
  restart. `deep_thinking_budget = 0` now means no cap.
  Checked by hand by the maintainer (2026-11-19): layout, live hotkey
  change, Apply and restart.
- Models window (`crates/macos/src/models_window.rs`, tray "Models…",
  "Manage models…" in Settings, or `glance app --open-models`): every
  model in models.toml with role, size, licence, repo, what uses it, and
  its state. Download (asks first: size, repo, pinned commit, licence, free
  space), Cancel, Check (re-hash against the pins), Repair (after a failed
  check), Delete (asks first; refused for models in use). Backend:
  `crates/app/src/cmd/app/models.rs`, the same pinned downloader and store
  as `glance models pull`, one job at a time; downloads need the missing
  size plus 2 GB free; a cancelled, failed or quit download's partial file
  is removed. `ModelStore::remove` deletes only the manifest's listed
  files. Not yet checked by hand.
- Personality (2026-11-19, `crates/orchestrator/src/personality.rs`):
  `[models] personality` = neutral (default) | friendly | direct | strict |
  custom, with `personality_custom` (up to 300 characters, cleaned to one
  line). A style layer appended after the FAST, escalation and DEEP system
  prompts; the prompts themselves are not edited. Neutral adds nothing
  (prompts byte-identical, tested). Every other layer ends with the same
  rule: plain spoken sentences, no emoji, no markdown, no lists. Length is
  untouched (no length words in any layer; the cap test passes with a
  personality). Settings: preset picker and custom field under Answers,
  live from the next chord press. The style and the length hint are pinned
  at the press, so a turn's prewarms and question always share one prompt.
  FAST eval (qwen3-vl-4b, 12 ordinary + 6 hard, temperature 0; answers in
  `eval-local/results/fast-persona-*.{json,md}`):

  | Preset | Ordinary correct | Ordinary words (median) | Hard words (median) |
  |---|---|---|---|
  | neutral (baseline) | 12/12 | 10 | 63 |
  | friendly, first wording | 11/12 | 12.5 | 58.5 |
  | friendly, final | 12/12 | 10 | 67.5 |
  | direct | 12/12 | 10 | 66.5 |
  | strict, first wording | 10/12 | 10 | 59 |
  | strict, final | 12/12 | 10 | 57.5 |
  | custom ("Calm and patient, like a good teacher.") | 12/12 | 13 | 65.5 |

  Friendly and strict at first dropped the exact type from the editor
  error ("u64"); both now ask for names, numbers and errors exactly as on
  screen. No emoji or markdown in any answer. On the 4B the styles are
  light (wording, not substance).
  DEEP cache after a change (`deep-bench --session --split-prewarm`, sheet,
  Qwen3.8-27B, follow-ups on one screenshot): neutral 2774 tokens (30.4 s,
  first look), neutral 150 (1.6 s), switch to friendly 3174 (34.5 s, one
  full re-prefill, history included), friendly 159 and 154 (1.7 s). A
  change costs DEEP one re-prefill on the next think-harder in a session,
  the same as a new screenshot; FAST re-reads the image once (~0.1 s).
  Fun presets (2026-11-19): pirate, caveman, shakespearean. A mild custom
  wording ("Talk like a pirate.") gave only an occasional "Aye" on the 4B
  (12/12); naming the character's words works. FAST eval, final wordings:
  pirate 12/12 (ordinary words median 24, hard 77), caveman 12/12 (9, 58.5),
  shakespearean 12/12 (hard 80), no cut-offs. First wordings had one
  answer each looping to the token limit ("aye, aye, …" with "aye" listed;
  "Me see …" with a "me see big number" example), so those were removed.
  Longer wording on the pirate and Shakespearean presets is flavour; the
  sentence cap still applies. DEEP with fun presets not evaluated.
- Maths as words (2026-11-19, `crates/orchestrator/src/math_words.rs`):
  `[speech] math_as_words` (default false), a Settings checkbox under Voice,
  live. Rewrites only the text sent to the voice ("2 x 2 = 4" is spoken
  "2 times 2 equals 4"); the model's answer, the overlay and so accuracy
  are unchanged. x, -, / and < > count only between numbers or single
  letters, so Wi-Fi, 1920x1080, dates and paths stay; tested on both.
  Found on the way, not caused by personality: a prewarm that goes back to
  a shorter prefix than DEEP has cached (e.g. a think-harder tap re-asking
  after a DEEP answer) re-prefills fully even with an unchanged prompt,
  because the hybrid model can only resume from a saved checkpoint
  (bench: 2695 of 2820 tokens). Not fixed; noted for M4.

- Visual redesign (2026-11-20; plan approved by the maintainer; layout and
  look only, no behaviour, hotkey or pipeline change):
  - Accent (`crates/macos/src/theme.rs`, the only colour values in
    glance-macos): light #D85A30, dark #EE7A50. WCAG: black text on accent
    5.42 (light) and 7.54 (dark); white would fail (3.87, 2.79), so text on
    accent is black. Accent against window backgrounds: 3.87/3.28 light,
    5.98/4.60 dark (non-text needs 3). Unit tests check these from the
    constants. `[appearance] accent = "glance" | "system"` (new key, asked
    for), live. Checkboxes, switches and focus rings always follow the macOS
    accent (an app needs an asset catalog, i.e. full Xcode, to change them).
    New direct dependency block2 0.6.2 (already in the tree).
  - Overlay (`crates/macos/src/overlay/`): popover-material blur, rounded
    corners, hairline border (solid with Reduce Transparency); status row
    with an accent glyph and label per state: Listening = waveform from the
    live mic level (`glance_audio::LevelMeter`, one number, never logged),
    Thinking/Transcribing = pulsing dots, Speaking = equaliser; question on
    a quieter line; long answers scroll and stay at their end. Fades and
    size eases on a ~30 Hz tick only while visible; none with Reduce
    Motion. Panel behaviour unchanged. `--overlay-demo` cycles all states
    twice. Ready (2026-11-20, maintainer feedback: the label floated after
    an empty gap): the waveform at rest, five flat pills at 60% of the
    accent, still on purpose (Ready is up while the answer is read); the
    bars come alive in place when listening starts.
  - Settings (`crates/macos/src/settings/`): sidebar window (Voice,
    Answers, Personality, Think harder, Models, Hotkeys, Appearance,
    About), Auto Layout, minimum 760×540, captioned rounded groups,
    "Restart to apply" on rows that need it, Restore defaults… per section
    (confirmed; `SettingsBackend::defaults()`), keycap hotkeys with the same
    rollback, the models window merged into the Models page (tray
    "Models…" opens it), About with version, licences and third-party
    notices. The old settings and models windows are removed.
  - `glance ui-snapshots` draws glance's own windows (demo text, longest
    names, light, dark and minimum size) to `eval-local/ui/`; never captures
    the screen. Blur is not drawn in-process (solid fallback shown).
  - Found and fixed on the way: two rare test races (stale-server temp
    dirs sharing a name; a mic-start count read before the worker ran).
  Not yet checked by hand: the real overlay over light and dark apps,
  Reduce Motion / Reduce Transparency, hotkey change and Restore defaults
  from the new window.

- Screenshot indicator (2026-11-25; plan approved by the maintainer;
  visual and UX only, no capture, model, hotkey or existing-config change):
  - `crates/macos/src/glow.rs`: on a new screenshot, a ~0.5 s accent glow
    around the edge of the display that was captured (its
    `CGDirectDisplayID` travels on `Screenshot`; unknown means the main
    screen). The panel is the overlay's kind (non-activating, click-through,
    every Space, over full-screen apps). Four thin edge views are drawn
    once; the fade is a Core Animation opacity animation (`NSAnimationContext`
    and the animator), no per-frame work, and the panel is off screen
    between flashes. The fade uses the default (linear) timing: setting a
    curve needs `objc2-quartz-core`, which was not added.
  - `[appearance] screenshot_indicator` = glow (default) | subtle (a thin
    line) | off; Settings > Appearance > Screenshot indicator, live, with a
    preview on the window's display. It works with the overlay on or off.
    Reduce Motion: a still line, held 0.5 s, one plain fade. Reduce
    Transparency: an opaque line, no translucent band.
  - Orchestrator: `Update::Screenshot` (no content). `Fresh` is sent from
    `prepare` once the capture has returned and before encoding, so the glow
    can never be in that image and a turn cancelled during the capture sends
    nothing. `Reused { taken }` is sent at the press of a follow-up or
    think-harder. `State(Listening)` now goes out before the capture starts,
    so the screenshot update always follows it.
  - Overlay: a quiet line under the status reads "New screenshot taken" or
    "Using screenshot from 12 s ago" (fixed at the press); it replaces the
    meta text's "screenshot N s old", which followed the session.
  - Proof it is not in the image: orchestrator tests (`Fresh` names the
    display of the capture that returned, comes after Listening, never on a
    follow-up, think-harder or failed capture), and `glance capture-check
    --indicator`. That holds the glow on screen and compares the outer 24 px
    with a capture taken without it: glance's capture must be unchanged
    (< 1.5 of 255) while a control capture with glance's windows left in must
    see it (> 10). It then plays one animated flash, which must show at
    0.12 s and be gone by 1.1 s. From a terminal (2026-11-25, three runs):
    excluded 0.00–0.02, control 48.0–51.6; flash 35.4–36.9 during,
    0.00–0.02 after. All PASS. Numbers only; no pixels kept.
  - `--overlay-demo`: the first round glows ("New screenshot taken"), the
    think-harder round does not ("Using screenshot from 13 s ago"), then a
    tour of the four looks. `ui-snapshots`: `glow-*.png` and an
    `overlay-speaking-reused-shot` state.
  - Not yet checked by hand: the demo itself; `capture-check --indicator`
    inside the signed bundle; a real chord press; the glow over a
    full-screen app and on another Space; the right display with two
    monitors; Reduce Motion and Reduce Transparency switched on; a live
    switch in Settings; focus and clicks unaffected during a glow.

Left: check the redesign and the screenshot indicator by hand; rebuild the
bundle and run the M2/M3 hands-on checks with the maintainer.

## M4 (in progress)

Done:
- `llm-client::escalate`: the streaming gate (DESIGN §7.3) and the GBNF
  grammar; 7 tests.
- `llm-client::server`: `ManagedServer` (127.0.0.1, random port, per-launch
  key in `LLAMA_API_KEY`, `/health` wait, `/props` vision check, output
  discarded, killed on drop). `glance server-check <id>` on Qwen3-VL-4B:
  started in 3.9 s; with the grammar and `stream: true`, an escalating
  request produced a valid signal (first token 241 ms) and a normal one a
  plain answer (203 ms).
- Orchestrator: escalation flow (filler by reason, DEEP with its own 2048 px
  copy, never gated, thinking never spoken, one escalation per turn, invalid
  signal speaks "Sorry, ask me again"), DEEP prewarm in the background after
  FAST's; 6 tests.
- App: `[models]` in `config.toml` (managed `fast`/`deep` ids or
  `*_endpoint`); `glance app` runs FAST itself by default; Quit stops the
  servers.

Direction (maintainer, 2026-11-18): the default is one small model (FAST
only); automatic escalation and DEEP are off by default (`auto_escalate`
in `[models]`); DEEP becomes on-demand (a "think harder" request or its own
hotkey) and unloads after an idle time. The MoE download is on hold.

Left:
1. Build DEEP on demand (plan to agree first). Qwen3.8-27B is pinned and
   tuned: 10 s to the first word with a 128-token thinking cap once its
   image is prewarmed (below).
2. A `glance bench` mode for escalated turns (filler and DEEP timings,
   format compliance with and without the grammar).
3. Measure whether DEEP's background prewarm slows FAST (DESIGN §4).
4. Pre-synthesize the filler lines when Kokoro is the voice.
5. Done (2026-11-18): servers left behind by a crash are stopped at the
   next start. Each managed server has a PID file in
   `~/Library/Application Support/glance/run/`; `glance app`, the benches and
   `server-check` read them first and stop a server whose glance is gone,
   after checking the PID is still a `llama-server --host 127.0.0.1 --port
   <that port>` (a reused PID is never touched). Tested with fake process
   tables (6 tests) and for real: SIGKILL of `glance app`, then the next
   start stopped the orphan and left the run directory empty.

**DEEP bake-off, Qwen3.8-27B Q4_K_M (bartowski), MTP on (`glance deep-bench`).**
Six hard eval questions, 2048 px image, FAST Qwen3-VL-4B alongside. FAST
escalated 6/6 in 0.27–0.30 s (grammar on). DEEP: 19.4 GB resident, 18–25
tok/s, MTP accepted 55–86% of drafted tokens. Times are median / max:

| Effort | Thinking | Answer | DEEP request → first word | Key-up → first word | Same, if the image were reused |
|---|---|---|---|---|---|
| low | 30.5 / 83.6 s | 6.1 / 10.5 s | 60.5 / 114.8 s | 60.9 / 115.2 s | ~31.6 / 87.9 s |
| medium | 20.4 / 57.7 s | 4.4 / 11.9 s | 50.0 / 91.9 s | 50.4 / 92.3 s | ~22.4 / 61.9 s |
| xhigh | 112.6 / 245.3 s | 6.3 / 14.6 s | 142.3 / 276.2 s | 142.7 / 276.6 s | ~103.4 / 249.2 s |

Budget is ~5 s from key-up. **No effort level is acceptable.** Two causes:
1. The prewarm is never reused: Qwen3.8 is a hybrid (attention + recurrent)
   model, llama.cpp restores such state only from checkpoints, and the
   checkpoint sits at the end of the prewarm, past where the question
   diverges. Every question re-evaluates ~2,720 tokens (28 s median);
   `--checkpoint-min-step 64` did not help.
2. Thinking alone takes 15–245 s. `medium` thought less than `low` on this
   set (fewer tokens in 5 of 6).

Answers are in `eval-local/results/deep-qwen3.8-27b.json` for spot-checking.

**After the fixes** (`deep-bench --split-prewarm`, MTP on, 2048 px, medium
effort unless noted; median / max over the 6 questions, prewarm finished):

| Setting | Thinking | Key-up → first word |
|---|---|---|
| Before (prewarm not reused, uncapped) | 20.4 / 57.7 s | 50.4 / 92.3 s |
| Prewarm reused: request → first thinking token | – | 29.3 s → 0.53 s (cache 105 → 2,774 tokens) |
| + thinking capped at 256 tokens | 13.4 / 14.9 s | 16.0 / 17.9 s |
| + thinking capped at 128 tokens | 7.2 / 7.7 s | **10.0 / 10.2 s** |
| Thinking off | – | 2.6 / 3.4 s |

Quality check on the two calculation questions (chart, sheet), against
uncapped xhigh: the 128 cap agreed on both; thinking off got one sheet figure
wrong and answered in markdown bullets.

DEEP's image prewarm, which a fresh-screenshot escalation waits for: 2048 px
28–30 s, 1536 px 15.6–16.5 s, 1024 px 6.8–7.7 s. At 1536 and 1024 px the
sheet answer was wrong (1024 px spoke a wrong first sentence, then corrected
itself), so DEEP stays at 2048 px. Consequence: a question escalated right
after a new `look` waits ~25 s more; follow-ups on the same screenshot do not.

Defaults now: medium effort, 128-token thinking cap, MTP on, 2048 px.

**FAST-only comparison, 4B vs 8B (`glance fast-bench`, 2026-11-18).** The
default setup (one small model, no DEEP). 6 ordinary + 6 hard eval
questions, each asked fresh (new image prewarmed at key-down, key held 3 s)
and warm (follow-up on the same screenshot); first word = key-up to the first
sentence sounding through Kokoro (volume 0), including 0.12 s STT. Context
8192. Medians / max over 12 per condition:

| Model | llama-server resident | + glance (STT + Kokoro, 1.74 GB) | Fresh first word | Warm first word | Image prewarm | Speed | Ordinary correct | Hard answer length |
|---|---|---|---|---|---|---|---|---|
| Qwen3-VL-4B | 5.3 GB | 7.0 GB | 1.23 / 1.70 s | 0.95 / 1.49 s | 1.36 s | 76 tok/s | 12/12 | 63 words |
| Qwen3-VL-8B | 8.1 GB | 9.8 GB | 1.07 / 1.67 s | 1.14 / 1.96 s | 2.29 s | 46 tok/s | 12/12 | 35 words |

Both prewarms finish inside the 3 s hold, so "fresh" costs no more than
"warm"; a question shorter than the prewarm waits for the rest of it (4B
1.4 s, 8B 2.3 s from key-down). First-word times are close because Kokoro
(0.47–0.60 s) and the first sentence's length dominate. No answer hit the
200-token cap. Answers side by side: `eval-local/results/fast-compare.md`.

**Think harder, cold (`deep-bench --cold`, 2026-11-18).** DEEP not loaded:
each of the 6 hard questions starts DEEP afresh (Qwen3.8-27B, medium effort,
128-token thinking cap, MTP, 2048 px), FAST running alongside. Median / max:
load 9.7 / 9.7 s (model file in the OS cache; reading 17.4 GB from the SSD
would add a few seconds), image prefill 28.8 / 29.4 s, thinking 7.2 / 7.5 s,
**tap to DEEP's first word 48.6 / 49.0 s**. Once loaded and prefilled, the
same question takes ~10 s; loaded but on a new screenshot ~39 s.

**Brevity prompt ("two or three short sentences", 2026-11-18): did not
shorten answers.** FAST 4B hard answers 63 → 70 words median (max 87 → 133,
one now cut off at the 200-token cap); ordinary 10 → 11. DEEP 155 → 128 words.
Two regressions: FAST's editor answer lost the expected detail (11/12
ordinary correct, was 12/12), and DEEP's sheet answer opened with a wrong
claim ("Engineering grew fastest") before correcting itself, likely from
"the conclusion first" under the 128-token thinking cap. Answers:
`eval-local/results/fast-brief.md`, `eval-local/results/deep-cold.json`.

**response_length (2026-11-18).** Original prompts restored; cap in code.
FAST 4B, the 12 eval questions asked fresh and warm (24 answers per mode);
DEEP (Qwen3.8-27B, medium effort, 128-token thinking cap) on sheet and chart:

| Mode | FAST hard words (median / max) | FAST ordinary words | Ordinary correct | FAST first word | DEEP words (sheet, chart) | DEEP sheet answer |
|---|---|---|---|---|---|---|
| short | 52 / 92 | 12 | 12/12 | 1.05 s | 32, 41 | **wrong** ("Engineering grew fastest") |
| medium | 63 / 89 | 10 | 12/12 | 1.09 s | 42, 46 | correct |
| long | 72 / 121 | 14 | 10/12 | 1.17 s | 65, 85 | correct |

Three of the six hard questions ask for a walkthrough or detail, so they are
never capped. The two long-mode misses are the editor question answered with
a different detail than the expected `u64` (fresh and warm). DEEP's chart
answer was right in all modes; its first word came 7.9–9.4 s after the
question with the screenshot prefilled. The earlier regressions (FAST's
editor answer, DEEP's wrong opening) are gone in medium.

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
glance capture-check --indicator             # the glow is never in a capture (numbers only)
glance app --overlay-demo                    # scripted turns + the indicator's looks
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
cargo test --workspace --locked              # 221 tests
cargo audit
cargo deny check                             # passes; one ignore, one exception
```
Eval screens and Kokoro spike: `README.md`, `spikes/README.md`.

## Next step: hands-on acceptance (needs the maintainer)

The M2/M3 steps below, plus the M5 windows (overlay states over light and
dark apps, Reduce Motion and Reduce Transparency, settings changes, think
harder), the screenshot indicator (its "Not yet checked by hand" list in M5,
starting with `open -W --stderr "$(tty)" target/bundle/Glance.app --args
capture-check --indicator`), then the public-release decisions above.

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
