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

## language-tokenizer 0.1.0 (pulled in by misaki-rs, unused)

Reviewed 2026-11-17 for a proposed `deny.toml` licence exception. The
exception was **not added**, because of the unsound `unsafe` below, so
`cargo deny check` fails on licences until the maintainer decides.

- Why it is here: misaki-rs 0.9.0 lists it as a normal dependency (with
  feature `snowball`), but nothing in misaki-rs's source uses it. Neither it
  nor `waken_snowball` has any symbol in the release binary (`nm`), while
  misaki-rs has 158: the linker drops it.
- Publisher: crates.io user `savannstm`, sole owner, published every version.
  Crate created 2026-01-07; 116k downloads; latest 0.4.2 (misaki-rs's `0.1`
  requirement keeps it on 0.1.0).
- Licence: no `license` field in 0.1.0's Cargo.toml, only
  `license-file = "LICENSE.md"`, which is the WTFPL v2 text
  (crates.io shows 0.1.0 as "non-standard"; 0.4.1+ declare `WTFPL`).
- Size: 136 kB, one 803-line `src/lib.rs`. No build script (`build = false`).
  Not a proc macro itself; depends on `strum_macros` (proc macro).
- No filesystem, network, process or environment access (only
  `include_str!("../README.md")` for docs).
- **Unsound `unsafe`**: two `transmute::<Algorithm, waken_snowball::Algorithm>`
  calls in `tokenize_snowball`. Its `Algorithm` is `#[repr(i8)]` with
  `None = -1` and CJK/SE-Asian variants; waken_snowball's has 33 variants and
  no `repr`, so its layout is unspecified. `is_snowball()` is true for
  `Algorithm::None` (the `#[default]`), so the safe public
  `tokenize(text, Algorithm::default(), _)` transmutes `-1` into an enum with
  no such value: undefined behaviour from safe code. glance never reaches it
  (misaki-rs never calls the crate), but it is compiled.
- Its new dependency `waken_snowball` 0.1.0 (BSD-3-Clause, no `unsafe`, no
  build script) is published by crates.io user `mc373906408` ("waken") while
  its manifest names "Snowball Contributors" and the official
  snowballstem/snowball repository: an unofficial repackaging.
