//! OpenAI-compatible streaming chat client behind the [`ChatModel`] trait.
//!
//! One of the two crates allowed to depend on an HTTP client (enforced in
//! `deny.toml`). Endpoints are loopback-only: there is no way to construct a
//! non-loopback [`Endpoint`] yet, and when one is added it must carry the
//! explicit opt-in described in `docs/DESIGN.md` §11.
//!
//! [`escalate`] parses FAST's escalation signal; [`server`] runs a managed
//! `llama-server` per tier.

mod client;
mod endpoint;
pub mod escalate;
pub mod server;
mod sse;
pub mod stale;
mod types;

use std::future::Future;
use std::pin::Pin;

pub use client::OpenAiClient;
pub use endpoint::{ApiKey, Endpoint, EndpointError};
pub use types::{
    ChatError, ChatEvent, ChatRequest, ChatStream, ChatSummary, Message, Part, Role, Timings,
    Usage, status_error,
};

/// A chat model that streams its reply. Implemented by [`OpenAiClient`] and by
/// mocks in orchestrator tests.
pub trait ChatModel: Send + Sync {
    /// Start a streamed completion. Dropping the returned [`ChatStream`]
    /// cancels generation.
    fn chat(
        &self,
        request: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ChatStream, ChatError>> + Send + '_>>;
}
