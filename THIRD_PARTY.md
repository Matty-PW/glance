# Third-party code and data with caveats

Dependency licences are enforced by `deny.toml` (allowlist, GPL family
denied). This file records the cases where a licence check alone does not tell
the whole story, so they are reviewed before glance is ever distributed as a
binary. Model licences are recorded per model in `models.toml`.

## Models

glance does not include or redistribute any model weights. The repository
holds only [`models.toml`](models.toml): for each model, the Hugging Face
repository, a pinned commit, and the size and SHA-256 of every file. Weights
are downloaded by the user, from that manifest only, into
`~/Library/Application Support/glance/models/`, and checked against their
pins before use. Each model stays under its own licence:

| glance id | Original model | Downloaded from | Licence |
|---|---|---|---|
| `qwen3-vl-4b-instruct-q4km` | Qwen3-VL-4B-Instruct (Qwen) | `Qwen/Qwen3-VL-4B-Instruct-GGUF` | Apache-2.0 |
| `qwen3-vl-8b-instruct-q4km` | Qwen3-VL-8B-Instruct (Qwen) | `Qwen/Qwen3-VL-8B-Instruct-GGUF` | Apache-2.0 |
| `qwen3.8-27b-q4km` | Qwen3.8-27B (Qwen) | `bartowski/Qwen3.8-27B-GGUF` (community GGUF conversion) | Apache-2.0 |
| `lfm2.5-vl-3b-q4km` | LFM2.5-VL-3B (Liquid AI) | `LiquidAI/LFM2.5-VL-3B-GGUF` | LFM Open License v1.0 (below) |
| `parakeet-tdt-0.6b-v3-int8` | parakeet-tdt-0.6b-v3 (NVIDIA) | `istupakov/parakeet-tdt-0.6b-v3-onnx` (int8 ONNX conversion) | CC-BY-4.0 (below) |
| `parakeet-tdt-0.6b-v2-int8` | parakeet-tdt-0.6b-v2 (NVIDIA) | `istupakov/parakeet-tdt-0.6b-v2-onnx` (int8 ONNX conversion) | CC-BY-4.0 (below) |
| `kokoro-82m-v1.0-*` (model, voices) | Kokoro-82M v1.0 (hexgrad) | `onnx-community/Kokoro-82M-v1.0-ONNX` | Apache-2.0 |

### Parakeet (CC-BY-4.0 attribution)

Speech recognition uses **parakeet-tdt-0.6b-v3** (and optionally v2) by
**NVIDIA**, licensed under the
[Creative Commons Attribution 4.0 International licence](https://creativecommons.org/licenses/by/4.0/).
glance uses the int8 ONNX conversion published by `istupakov` on Hugging
Face; glance itself does not modify the model, and the user downloads it
from the pinned manifest. The original models are published by NVIDIA as
`nvidia/parakeet-tdt-0.6b-v3` and `nvidia/parakeet-tdt-0.6b-v2`.

### LFM Open License v1.0 (optional LFM2.5-VL-3B model)

Read at the pinned revision (`LiquidAI/LFM2.5-VL-3B-GGUF`,
`6f730e9a2c454e8af9adc29db58e638e01e5957f`, file `LICENSE`). In plain
words:

- You may use, copy, modify and share the model free of charge.
- If you share it (or a changed version), include a copy of the licence,
  mark the files you changed, and keep the original notices.
- **Commercial use is licensed only while you and the companies you
  control, are controlled by, or share control with have under
  USD 10 million in annual revenue.** Above that, commercial use is not
  covered. (The licence says "not exceeding" in one place and "10 million
  or more" in another, so exactly USD 10 million is ambiguous.) Qualified
  non-profits are exempt for non-commercial or research use.
- Suing anyone over patents in the model ends your licence, and so does
  breaking its terms (you must then stop using it and delete your copies).
- No trademark rights; no warranty.

The model is optional and not a default; glance only downloads it when the
user asks.

## Dependency licences beyond MIT and Apache-2.0

`cargo deny` allows only permissive licences (see `deny.toml`). Besides MIT
and Apache-2.0, the dependency tree contains BSD-2-Clause, BSD-3-Clause, ISC,
Zlib, Unicode-3.0, and crates offered under a choice that includes MIT or
Apache-2.0 (0BSD, MIT-0, CC0-1.0, Unlicense). Two exceptions are listed in
`deny.toml` by name: `webpki-root-certs` (CDLA-Permissive-2.0, Mozilla's root
certificate bundle) and `language-tokenizer@0.1.0` (WTFPL, below).

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

Reviewed 2026-11-17 for a `deny.toml` licence exception. **Decision
(maintainer, 2026-11-17): exception added** for exactly
`language-tokenizer@0.1.0` (WTFPL), with the problems below accepted because
the crate is never called and not linked. Any other version needs a fresh
review. Exit plan: ask misaki-rs to drop the unused dependency, then remove
the exception. `cargo deny` still prints a `no-license-field` warning for it
(it has only a licence file); that is expected.

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
- Its dependency `waken_snowball` 0.1.0: BSD-3-Clause, `build = false`, no
  dependencies, no proc macros, no `unsafe` (manifest reviewed by the
  maintainer too: clean). Misleading metadata: it is published by crates.io
  user `mc373906408` ("waken") while its manifest names "Snowball
  Contributors" as author and the official snowballstem/snowball repository,
  so it is an unofficial repackaging of Snowball's stemmers.
