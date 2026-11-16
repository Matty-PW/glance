//! OpenAI-compatible streaming chat client behind the `ChatModel` trait, the
//! strict escalation parser, and the managed `llama-server` supervisor.
//!
//! One of the two crates allowed to depend on an HTTP client (enforced in
//! `deny.toml`). Endpoints are loopback-only unless explicitly opted in.
//! See `docs/DESIGN.md` §7 and §11.
