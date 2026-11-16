# glance

Hold a hotkey, ask a question out loud about whatever is on your screen, and hear
a local vision model answer. macOS on Apple Silicon only for v1. Everything runs
on your machine: no telemetry, no cloud by default.

Status: skeleton. Nothing is wired up yet. The design, including the milestone
plan, is in [docs/DESIGN.md](docs/DESIGN.md). Project rules are in
[CLAUDE.md](CLAUDE.md).

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
