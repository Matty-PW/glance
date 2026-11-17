//! Request and stream types shared by every [`crate::ChatModel`].

use std::fmt;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

/// One piece of a message. Image bytes never appear in `Debug` output.
#[derive(Clone)]
pub enum Part {
    Text(String),
    /// PNG bytes, sent inline as a data URI. Never a remote URL: the server
    /// would fetch it.
    Png(Arc<[u8]>),
}

impl fmt::Debug for Part {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Part::Text(t) => write!(f, "Text({} chars)", t.chars().count()),
            Part::Png(b) => write!(f, "Png({} bytes)", b.len()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub parts: Vec<Part>,
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            parts: vec![Part::Text(text.into())],
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            parts: vec![Part::Text(text.into())],
        }
    }

    pub fn user(parts: Vec<Part>) -> Self {
        Self {
            role: Role::User,
            parts,
        }
    }

    pub fn user_text(text: impl Into<String>) -> Self {
        Self::user(vec![Part::Text(text.into())])
    }

    fn to_json(&self) -> Value {
        let content = match self.parts.as_slice() {
            [Part::Text(text)] => Value::String(text.clone()),
            parts => Value::Array(
                parts
                    .iter()
                    .map(|part| match part {
                        Part::Text(text) => json!({"type": "text", "text": text}),
                        Part::Png(bytes) => json!({
                            "type": "image_url",
                            "image_url": {
                                "url": format!("data:image/png;base64,{}", STANDARD.encode(bytes))
                            }
                        }),
                    })
                    .collect(),
            ),
        };
        json!({"role": self.role.as_str(), "content": content})
    }
}

#[derive(Debug, Clone, Default)]
pub struct ChatRequest {
    pub messages: Vec<Message>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    /// Server-specific extras merged into the request body last, for example
    /// llama-server's `cache_prompt` or `chat_template_kwargs`.
    pub extra_body: Map<String, Value>,
}

impl ChatRequest {
    pub(crate) fn to_body(&self, model: &str) -> Value {
        let mut body = Map::new();
        body.insert("model".into(), json!(model));
        body.insert(
            "messages".into(),
            Value::Array(self.messages.iter().map(Message::to_json).collect()),
        );
        body.insert("stream".into(), json!(true));
        body.insert("stream_options".into(), json!({"include_usage": true}));
        if let Some(max) = self.max_tokens {
            body.insert("max_tokens".into(), json!(max));
        }
        if let Some(t) = self.temperature {
            body.insert("temperature".into(), json!(t));
        }
        for (key, value) in &self.extra_body {
            body.insert(key.clone(), value.clone());
        }
        Value::Object(body)
    }
}

/// llama-server's per-request timing block. Other servers omit it.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct Timings {
    /// Prompt tokens reused from the server's cache.
    pub cache_n: Option<u64>,
    /// Prompt tokens actually evaluated for this request.
    pub prompt_n: Option<u64>,
    pub prompt_ms: Option<f64>,
    pub prompt_per_second: Option<f64>,
    pub predicted_n: Option<u64>,
    pub predicted_ms: Option<f64>,
    pub predicted_per_second: Option<f64>,
    /// Speculative decoding (e.g. MTP): tokens drafted and accepted.
    pub draft_n: Option<u64>,
    pub draft_n_accepted: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct ChatSummary {
    pub finish_reason: Option<String>,
    pub timings: Option<Timings>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone)]
pub enum ChatEvent {
    /// Reply text to display and speak.
    Content(String),
    /// Thinking text from reasoning models. Never spoken.
    Reasoning(String),
    /// Always the last event of a successful stream.
    Done(ChatSummary),
}

#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error("could not reach the model endpoint: {0}")]
    Connect(#[source] reqwest::Error),
    #[error("model endpoint returned HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("stream from the model endpoint broke: {0}")]
    Stream(#[source] reqwest::Error),
    #[error("model endpoint sent something that is not a chat stream: {0}")]
    Protocol(String),
}

/// A streamed reply. Dropping it cancels generation on the server.
#[derive(Debug)]
pub struct ChatStream {
    pub(crate) rx: mpsc::Receiver<Result<ChatEvent, ChatError>>,
}

impl ChatStream {
    /// Next event, or `None` once the stream has ended.
    pub async fn next(&mut self) -> Option<Result<ChatEvent, ChatError>> {
        self.rx.recv().await
    }

    /// Build a stream from a channel. Lets mocks implement
    /// [`crate::ChatModel`] without any HTTP.
    pub fn from_channel(rx: mpsc::Receiver<Result<ChatEvent, ChatError>>) -> Self {
        Self { rx }
    }
}

// ---- wire format of one streamed chunk -------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct Chunk {
    #[serde(default)]
    pub choices: Vec<Choice>,
    pub timings: Option<Timings>,
    pub usage: Option<RawUsage>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Choice {
    #[serde(default)]
    pub delta: Delta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct Delta {
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawUsage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub prompt_tokens_details: Option<PromptDetails>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PromptDetails {
    pub cached_tokens: Option<u64>,
}

impl From<RawUsage> for Usage {
    fn from(raw: RawUsage) -> Self {
        Usage {
            prompt_tokens: raw.prompt_tokens,
            completion_tokens: raw.completion_tokens,
            cached_tokens: raw.prompt_tokens_details.and_then(|d| d.cached_tokens),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_comes_before_text_and_is_a_data_uri() {
        let request = ChatRequest {
            messages: vec![
                Message::system("be brief"),
                Message::user(vec![
                    Part::Png(Arc::from(&b"\x89PNG"[..])),
                    Part::Text("what is this?".into()),
                ]),
            ],
            max_tokens: Some(32),
            ..Default::default()
        };
        let body = request.to_body("fast");
        assert_eq!(body["model"], "fast");
        assert_eq!(body["stream"], true);
        assert_eq!(body["max_tokens"], 32);
        assert_eq!(body["messages"][0]["content"], "be brief");
        let parts = body["messages"][1]["content"].as_array().unwrap();
        assert_eq!(parts[0]["type"], "image_url");
        assert_eq!(
            parts[0]["image_url"]["url"],
            "data:image/png;base64,iVBORw=="
        );
        assert_eq!(parts[1]["text"], "what is this?");
    }

    #[test]
    fn extra_body_is_merged_last() {
        let mut request = ChatRequest::default();
        request
            .extra_body
            .insert("cache_prompt".into(), json!(true));
        request.extra_body.insert("stream".into(), json!(false));
        let body = request.to_body("m");
        assert_eq!(body["cache_prompt"], true);
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn debug_output_never_contains_image_or_text_content() {
        let message = Message::user(vec![
            Part::Png(Arc::from(&b"secret-pixels"[..])),
            Part::Text("secret words".into()),
        ]);
        let shown = format!("{message:?}");
        assert!(!shown.contains("secret"), "{shown}");
        assert!(shown.contains("Png(13 bytes)"));
    }
}
