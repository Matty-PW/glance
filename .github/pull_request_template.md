## What and why

<!-- What this changes, and why. Link the issue if there is one. -->

## How it was tested

<!-- Tests added or run, benchmarks, `glance ui-snapshots` for UI changes, a
real run through the app if it touches the turn. -->

## Checklist

- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings` and `cargo test --workspace --locked` pass
- [ ] `cargo deny check` and `cargo audit` pass
- [ ] Privacy rules hold: no screenshots, audio, transcripts or answers written to disk or logs; no new network access (see SECURITY.md)
- [ ] No new dependency, or each new one is explained below (what it is, why, licence, what it pulls in) and pinned exactly
- [ ] No model weights, images outside `docs/images/`, secrets or personal paths committed
- [ ] Platform code stays in `crates/macos`, behind the existing traits

## New dependencies

<!-- None, or: crate, version, licence, why. -->
