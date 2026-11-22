//! The HTTP implementation of [`ChatModel`].

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use reqwest::redirect::Policy;
use tokio::sync::mpsc;

use crate::ChatModel;
use crate::endpoint::Endpoint;
use crate::sse::SseParser;
use crate::types::{ChatError, ChatEvent, ChatRequest, ChatStream, ChatSummary, Chunk};

/// Talks to one OpenAI-compatible endpoint (llama-server, Ollama, LM Studio).
#[derive(Debug, Clone)]
pub struct OpenAiClient {
    http: reqwest::Client,
    endpoint: Endpoint,
}

impl OpenAiClient {
    pub fn new(endpoint: Endpoint) -> Result<Self, ChatError> {
        // No proxies (a proxy env var must not route screen content off the
        // machine) and no redirects (a local server must not bounce us to
        // another host).
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(300));
        if let Some((host, addr)) = &endpoint.pinned {
            builder = builder.resolve(host, *addr);
        }
        let http = builder.build().map_err(ChatError::Connect)?;
        Ok(Self { http, endpoint })
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    async fn start(&self, request: ChatRequest) -> Result<ChatStream, ChatError> {
        let body = request.to_body(self.endpoint.model());
        let mut post = self.http.post(self.endpoint.chat_url().clone()).json(&body);
        if let Some(key) = self.endpoint.api_key() {
            post = post.bearer_auth(key.expose());
        }
        let response = post.send().await.map_err(ChatError::Connect)?;

        let status = response.status();
        if !status.is_success() {
            // Read only to recognise a known error; the text is not kept.
            let body = response.text().await.unwrap_or_default();
            return Err(crate::types::status_error(status.as_u16(), &body));
        }

        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(pump(response, tx));
        Ok(ChatStream { rx })
    }
}

impl ChatModel for OpenAiClient {
    fn chat(
        &self,
        request: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ChatStream, ChatError>> + Send + '_>> {
        Box::pin(self.start(request))
    }
}

/// Read the SSE body and forward events. Ends, dropping the HTTP response and
/// with it the server-side generation, as soon as the receiver is dropped.
async fn pump(mut response: reqwest::Response, tx: mpsc::Sender<Result<ChatEvent, ChatError>>) {
    let mut parser = SseParser::default();
    let mut summary = ChatSummary::default();

    loop {
        let bytes = match response.chunk().await {
            Ok(Some(bytes)) => bytes,
            Ok(None) => break,
            Err(e) => {
                let _ = tx.send(Err(ChatError::Stream(e))).await;
                return;
            }
        };
        let payloads = match parser.push(&bytes) {
            Ok(p) => p,
            Err(_) => {
                let _ = tx.send(Err(ChatError::Protocol("unterminated line"))).await;
                return;
            }
        };
        for payload in payloads {
            if payload == "[DONE]" {
                let _ = tx.send(Ok(ChatEvent::Done(summary))).await;
                return;
            }
            let events = match absorb(&payload, &mut summary) {
                Ok(events) => events,
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    return;
                }
            };
            for event in events {
                if tx.send(Ok(event)).await.is_err() {
                    return; // receiver dropped: cancel
                }
            }
        }
    }
    // Stream ended without [DONE]; still report what we have.
    let _ = tx.send(Ok(ChatEvent::Done(summary))).await;
}

/// Parse one `data:` payload, fold timings/usage into `summary`, and return
/// the events it carries.
pub(crate) fn absorb(
    payload: &str,
    summary: &mut ChatSummary,
) -> Result<Vec<ChatEvent>, ChatError> {
    // serde's message can quote the value it rejected (model output), so
    // only its category is kept.
    let chunk: Chunk = serde_json::from_str(payload).map_err(|e| {
        ChatError::Protocol(match e.classify() {
            serde_json::error::Category::Syntax => "a chunk is not valid JSON",
            serde_json::error::Category::Data => "a chunk has an unexpected shape",
            serde_json::error::Category::Eof => "a chunk was cut short",
            serde_json::error::Category::Io => "a chunk could not be read",
        })
    })?;
    if let Some(timings) = chunk.timings {
        summary.timings = Some(timings);
    }
    if let Some(usage) = chunk.usage {
        summary.usage = Some(usage.into());
    }
    let mut events = Vec::new();
    for choice in chunk.choices {
        if let Some(reason) = choice.finish_reason {
            summary.finish_reason = Some(reason);
        }
        if let Some(text) = choice.delta.reasoning_content.filter(|t| !t.is_empty()) {
            events.push(ChatEvent::Reasoning(text));
        }
        if let Some(text) = choice.delta.content.filter(|t| !t.is_empty()) {
            events.push(ChatEvent::Content(text));
        }
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_reasoning_and_final_stats_are_extracted() {
        let mut summary = ChatSummary::default();

        let first = absorb(
            r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":null}}]}"#,
            &mut summary,
        )
        .unwrap();
        assert!(first.is_empty());

        let think = absorb(
            r#"{"choices":[{"delta":{"reasoning_content":"hmm"}}]}"#,
            &mut summary,
        )
        .unwrap();
        assert!(matches!(think.as_slice(), [ChatEvent::Reasoning(t)] if t == "hmm"));

        let text = absorb(
            r#"{"choices":[{"delta":{"content":"Hello"}}]}"#,
            &mut summary,
        )
        .unwrap();
        assert!(matches!(text.as_slice(), [ChatEvent::Content(t)] if t == "Hello"));

        let last = absorb(
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":900,"completion_tokens":7,
                         "prompt_tokens_details":{"cached_tokens":880}},
                "timings":{"cache_n":880,"prompt_n":20,"prompt_ms":41.5,
                           "predicted_n":7,"predicted_per_second":63.2}}"#,
            &mut summary,
        )
        .unwrap();
        assert!(last.is_empty());
        assert_eq!(summary.finish_reason.as_deref(), Some("stop"));
        let timings = summary.timings.unwrap();
        assert_eq!(timings.cache_n, Some(880));
        assert_eq!(timings.prompt_n, Some(20));
        assert_eq!(summary.usage.unwrap().cached_tokens, Some(880));
    }

    #[test]
    fn status_errors_never_keep_the_body() {
        let body = "{\"error\": \"bad request: what does SECRET-PROMPT mean on screen?\"}";
        let err = crate::types::status_error(400, body);
        let text = err.to_string();
        assert_eq!(text, "model endpoint returned HTTP 400");
        assert!(!format!("{err:?}").contains("SECRET"), "{err:?}");
        let loading = crate::types::status_error(503, r#"{"error":{"message":"Loading model"}}"#);
        assert_eq!(
            loading.to_string(),
            "model endpoint returned HTTP 503: the model is still loading"
        );
    }

    #[test]
    fn stream_errors_never_quote_the_stream() {
        let mut summary = ChatSummary::default();
        // A chunk with the wrong shape: serde would name the string it saw.
        let bad = r#"{"choices": "SECRET answer about Priyanka's invoice"}"#;
        let err = absorb(bad, &mut summary).unwrap_err().to_string();
        for word in ["SECRET", "Priyanka", "invoice"] {
            assert!(!err.contains(word), "{err}");
        }
        let err = absorb(r#"{"choices": [{"delta": {"content": 42}}]}"#, &mut summary)
            .unwrap_err()
            .to_string();
        assert!(!err.contains("42"), "{err}");
    }

    #[test]
    fn garbage_is_a_protocol_error() {
        let mut summary = ChatSummary::default();
        assert!(matches!(
            absorb("<html>not json</html>", &mut summary),
            Err(ChatError::Protocol(_))
        ));
    }
}
