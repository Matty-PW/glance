# Spikes

Throwaway experiments kept for reference. They are excluded from the Cargo
workspace, are not built by CI, and are not held to the app's rules beyond
"no secrets, no model files, no audio in git".

## kokoro-g2p (M1, time-boxed phonemizer spike)

Checks that `misaki-rs` (no espeak) produces phonemes Kokoro-82M accepts, and
tries a home-grown fallback for words missing from its dictionary
(`src/fallback.rs`: clock times, file paths, compound words, letter-to-sound).
Results are in `docs/PROGRESS.md`.

```sh
glance models pull kokoro-82m-v1.0-fp32      # from the repo root
cd spikes/kokoro-g2p
# Synthesize sentences.txt to WAV files (24 kHz) and print timing:
cargo run --release --bin misaki-spike -- \
  "$HOME/Library/Application Support/glance/models/kokoro-82m-v1.0-fp32" \
  onnx/model.onnx af_heart us ../../eval-local/kokoro
# Show phonemes with and without the fallback:
cargo run --release --bin dbg -- "Lunch is at 12:15 in manifest.rs."
```

`THREADS=<n>` sets ONNX Runtime's intra-op thread count. Output goes to
`eval-local/`, which is gitignored.
