# glance

Hold a hotkey, ask a question out loud about whatever is on your screen, and hear
a local vision model answer. macOS on Apple Silicon only for v1. Everything runs
on your machine: no telemetry, no cloud by default.

Status: M0 (CLI spike). There is no hotkey or app bundle yet; the pieces run
from the command line. The design, including the milestone plan and measured
latencies, is in [docs/DESIGN.md](docs/DESIGN.md). Project rules are in
[CLAUDE.md](CLAUDE.md).

## Try the spike

```sh
brew install llama.cpp                       # provides llama-server
alias glance='cargo run --release -q -p glance --'

glance models pull qwen3-vl-4b-instruct-q4km # 3.3 GB vision model, SHA-256 verified
glance models pull parakeet-tdt-0.6b-v3-int8 # 0.7 GB speech model
glance serve qwen3-vl-4b-instruct-q4km       # verifies, then runs llama-server on
                                             # 127.0.0.1 and prints the endpoint
```

In a second terminal, with any screenshot saved as a PNG:

```sh
glance spike --png shot.png --endpoint http://127.0.0.1:<port>/v1
```

It records the microphone for five seconds (macOS asks for Microphone access
for your terminal the first time), transcribes locally, asks the model about
the screenshot, prints the reply as it streams, and reports the time spent in
each stage. Nothing is written to disk and nothing leaves the machine.

Other commands: `glance models list|verify`, `glance mic-check`,
`glance cache-check`, `glance stt-bench`, `glance bench` (see `--help`).

## Development setup

```sh
brew install gitleaks cargo-audit cargo-deny
git config core.hooksPath .githooks   # once per clone: enables the pre-commit hook
```

The pre-commit hook refuses commits containing secrets, home-directory paths,
model weights, audio, images, or large files. The Rust toolchain version is
pinned in `rust-toolchain.toml`; rustup installs it on first use.

## Checks

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo audit
cargo deny check
```

CI runs the same checks plus a full-history secret scan.

## Evaluation set

`eval/` holds six mock application screens (HTML) with questions and expected
answers, so model comparisons use no real screen content. Render them once,
then point `glance bench` at a running endpoint:

```sh
mkdir -p eval-local/shots
for s in editor sheet mail terminal settings chart; do
  swift eval/render.swift eval/scenes/$s.html eval-local/shots/$s.png
done
glance bench --endpoint http://127.0.0.1:<port>/v1 --model <id> --out eval-local/results/<id>.json
```

`eval-local/` is gitignored.
