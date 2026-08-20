use serde::{Deserialize, Serialize};

use crate::ids::CallId;
use crate::usage::TokenUsage;

/// A user-authored input part.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserPart {
    Text { text: String },
    File { path: String },
}

/// One part of a stored message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    ToolCall {
        id: CallId,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        id: CallId,
        content: String,
        is_error: bool,
    },
    File {
        path: String,
    },
    Finish {
        reason: FinishReason,
        usage: TokenUsage,
    },
}

/// An incremental update to a part under construction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PartDelta {
    Text { delta: String },
    Reasoning { delta: String },
    ToolInput { json: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Canceled,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}
