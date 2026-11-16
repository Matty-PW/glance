//! OpenAI-compatible streaming chat client behind the [`ChatModel`] trait.
//!
//! One of the two crates allowed to depend on an HTTP client (enforced in
//! `deny.toml`). Endpoints are loopback-only: there is no way to construct a
//! non-loopback [`Endpoint`] yet, and when one is added it must carry the
//! explicit opt-in described in `docs/DESIGN.md` §11.
//!
//! The escalation parser and the managed `llama-server` supervisor land in M4.

mod client;
mod endpoint;
mod sse;
mod types;

use std::future::Future;
use std::pin::Pin;

pub use client::OpenAiClient;
pub use endpoint::{Endpoint, EndpointError};
pub use types::{
    ChatError, ChatEvent, ChatRequest, ChatStream, ChatSummary, Message, Part, Role, Timings, Usage,
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
