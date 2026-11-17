# Third-party code and data with caveats

Dependency licences are enforced by `deny.toml` (allowlist, GPL family
denied). This file records the cases where a licence check alone does not tell
the whole story, so they are reviewed before glance is ever distributed as a
binary. Model licences are recorded per model in `models.toml`.

## misaki-rs 0.9.0 (phonemizer for Kokoro)

- Crate licence: MIT (`Cargo.toml`: `license = "MIT"`; `LICENSE`: "MIT
  License, Copyright (c) 2026 Michele Yin"). Used with
  `default-features = false`, so the `espeak` feature, and with it any
  eSpeak NG code (GPL-3.0), is not built or linked.
- Embedded data, compiled into the binary: pronunciation dictionaries
  `data/us_gold.json`, `us_silver.json`, `gb_gold.json`, `gb_silver.json`,
  and `data/tagger.bin`.
- **Provenance caveat (dictionaries).** The crate's README says, in its
  "Pronunciations" section (line 134): "The original misaki project had very
  few words and some were not pronunced correctly. Here I updated the
  original pronunciation dict to include more words and correct
  pronunciations using eSpeak." Its "License" section (line 154) says: "This
  project is based on the original Misaki library. See the original repository
  for licensing details regarding the underlying dictionary data." The
  upstream repository (hexgrad/misaki) is Apache-2.0 and does not state where
  its dictionaries come from. So the dictionary data has no licence statement
  of its own, and part of it was produced with eSpeak NG, whose program and
  pronunciation data are GPL-3.0. Whether dictionary entries generated with
  eSpeak are derived from its GPL data is unclear.
- `data/tagger.bin`: weights of spaCy's `en_core_web_sm` part-of-speech
  tagger (per the crate's `src/spacy_tagger.rs`), MIT according to that
  model's card. Raw little-endian f32, compiled in with `include_bytes!` and
  read positionally; no deserializer.
- Decision (maintainer, 2026-11-17): use misaki-rs for local builds. If
  glance is distributed as a binary and the data licence is a problem, swap
  the phonemizer: it sits behind the `Phonemizer` trait in `crates/tts`, and
  the planned replacement is a CMUdict-based G2P (`docs/DESIGN.md` §6.3).
  That path is not built.
