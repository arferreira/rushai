//! LLM provider clients.
//!
//! Each provider translates a [`ChatRequest`] into its own wire protocol and
//! streams back [`ProviderEvent`]s. Per-provider quirks (cache breakpoints,
//! reasoning signatures, endpoint routing) stay inside the impl.

mod anthropic;
pub mod copilot;
pub mod fake;
mod openai_compat;
mod retry;

use std::pin::Pin;

use futures::Stream;
use rushai_protocol::{CallId, Part, Role, TokenUsage};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use anthropic::Anthropic;
pub use copilot::{Copilot, CopilotAuth, DeviceCode};
pub use openai_compat::OpenAiCompat;
pub use retry::Retrying;

#[derive(Debug, Clone, PartialEq)]
pub struct ModelInfo {
    /// Provider-side model id, e.g. `claude-opus-5`.
    pub id: String,
    pub context_window: u64,
    pub max_output: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatMessage {
    pub role: Role,
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChatRequest {
    pub system: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolDef>,
    /// Defaults to the model's max output.
    pub max_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ProviderEvent {
    TextDelta(String),
    Reasoning {
        text: String,
        /// Set when the provider signs reasoning; must round-trip on replay.
        signature: Option<String>,
    },
    ToolCallStart {
        id: CallId,
        name: String,
    },
    ToolCallDelta {
        id: CallId,
        json: String,
    },
    ToolCallEnd {
        id: CallId,
    },
    Usage(TokenUsage),
    Done {
        stop: StopReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Other(String),
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("api error {status}: {message}")]
    Api {
        status: u16,
        message: String,
        /// Parsed from a Retry-After header when the server sent one.
        retry_after: Option<std::time::Duration>,
    },
    #[error("stream error: {0}")]
    Stream(String),
    #[error("unexpected provider event: {0}")]
    Protocol(String),
}

pub type EventStream = Pin<Box<dyn Stream<Item = Result<ProviderEvent, ProviderError>> + Send>>;

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    fn model(&self) -> &ModelInfo;
    async fn stream(&self, request: &ChatRequest) -> Result<EventStream, ProviderError>;
}

/// Connect timeout catches dead hosts; the read timeout only fires when a
/// stream stalls between chunks, so long generations are unaffected. No
/// whole-request timeout: streams legitimately run for minutes.
pub(crate) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .read_timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("default client config is valid")
}
