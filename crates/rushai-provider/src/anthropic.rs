use std::collections::HashMap;

use async_stream::try_stream;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use rushai_protocol::{CallId, Part, Role, TokenUsage};
use serde_json::{Value, json};

use crate::{
    ChatRequest, EventStream, ModelInfo, Provider, ProviderError, ProviderEvent, StopReason,
};

const API_VERSION: &str = "2023-06-01";

pub struct Anthropic {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: ModelInfo,
}

impl Anthropic {
    pub fn new(api_key: String, model: ModelInfo) -> Self {
        Self::with_base_url(api_key, model, "https://api.anthropic.com".into())
    }

    pub fn with_base_url(api_key: String, model: ModelInfo, base_url: String) -> Self {
        Self {
            client: crate::http_client(),
            base_url,
            api_key,
            model,
        }
    }

    fn body(&self, request: &ChatRequest) -> Value {
        let mut tools: Vec<Value> = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.input_schema,
                })
            })
            .collect();
        // Cache breakpoints: system, the tool list, and the last two messages.
        // Everything before a breakpoint becomes a stable prefix across turns.
        if let Some(last) = tools.last_mut() {
            last["cache_control"] = cache_control();
        }
        let count = request.messages.len();
        let messages: Vec<Value> = request
            .messages
            .iter()
            .enumerate()
            .map(|(i, message)| {
                let mut blocks: Vec<Value> = message.parts.iter().filter_map(block).collect();
                if i + 2 >= count
                    && let Some(last) = blocks.last_mut()
                {
                    last["cache_control"] = cache_control();
                }
                json!({
                    "role": match message.role {
                        Role::User => "user",
                        Role::Assistant => "assistant",
                    },
                    "content": blocks,
                })
            })
            .collect();

        let mut body = json!({
            "model": self.model.id,
            "max_tokens": request.max_tokens.unwrap_or(self.model.max_output),
            "stream": true,
            "messages": messages,
        });
        if !request.system.is_empty() {
            body["system"] = json!([{
                "type": "text",
                "text": request.system,
                "cache_control": cache_control(),
            }]);
        }
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools);
        }
        body
    }
}

fn cache_control() -> Value {
    json!({ "type": "ephemeral" })
}

fn block(part: &Part) -> Option<Value> {
    match part {
        Part::Text { text } => Some(json!({ "type": "text", "text": text })),
        Part::Reasoning { text, signature } => Some(json!({
            "type": "thinking",
            "thinking": text,
            "signature": signature.as_deref().unwrap_or(""),
        })),
        Part::ToolCall { id, name, input } => Some(json!({
            "type": "tool_use",
            "id": id.as_str(),
            "name": name,
            "input": input,
        })),
        Part::ToolResult {
            id,
            content,
            is_error,
        } => Some(json!({
            "type": "tool_result",
            "tool_use_id": id.as_str(),
            "content": content,
            "is_error": is_error,
        })),
        Part::File { .. } | Part::Finish { .. } => None,
    }
}

#[async_trait::async_trait]
impl Provider for Anthropic {
    fn model(&self) -> &ModelInfo {
        &self.model
    }

    async fn stream(&self, request: &ChatRequest) -> Result<EventStream, ProviderError> {
        let response = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&self.body(request))
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let retry_after = crate::retry::parse_retry_after(response.headers());
            let message = response.text().await.unwrap_or_default();
            return Err(ProviderError::Api {
                status: status.as_u16(),
                message,
                retry_after,
            });
        }

        let mut sse = response.bytes_stream().eventsource();
        let stream = try_stream! {
            let mut blocks: HashMap<u64, BlockKind> = HashMap::new();
            let mut stop = StopReason::EndTurn;
            while let Some(event) = sse.next().await {
                let event = event.map_err(|e| ProviderError::Stream(e.to_string()))?;
                let data: Value = serde_json::from_str(&event.data)
                    .map_err(|e| ProviderError::Protocol(format!("bad event json: {e}")))?;
                match data["type"].as_str().unwrap_or_default() {
                    "message_start" => {
                        yield ProviderEvent::Usage(usage(&data["message"]["usage"]));
                    }
                    "content_block_start" => {
                        let index = data["index"].as_u64().unwrap_or_default();
                        let block = &data["content_block"];
                        match block["type"].as_str().unwrap_or_default() {
                            "tool_use" => {
                                let id = CallId::from(block["id"].as_str().unwrap_or_default());
                                let name = block["name"].as_str().unwrap_or_default().to_owned();
                                blocks.insert(index, BlockKind::Tool(id.clone()));
                                yield ProviderEvent::ToolCallStart { id, name };
                            }
                            kind => {
                                blocks.insert(index, BlockKind::other(kind));
                            }
                        }
                    }
                    "content_block_delta" => {
                        let index = data["index"].as_u64().unwrap_or_default();
                        let delta = &data["delta"];
                        match delta["type"].as_str().unwrap_or_default() {
                            "text_delta" => {
                                let text = delta["text"].as_str().unwrap_or_default().to_owned();
                                yield ProviderEvent::TextDelta(text);
                            }
                            "thinking_delta" => {
                                let text =
                                    delta["thinking"].as_str().unwrap_or_default().to_owned();
                                yield ProviderEvent::Reasoning { text, signature: None };
                            }
                            "signature_delta" => {
                                let signature =
                                    delta["signature"].as_str().unwrap_or_default().to_owned();
                                yield ProviderEvent::Reasoning {
                                    text: String::new(),
                                    signature: Some(signature),
                                };
                            }
                            "input_json_delta" => {
                                if let Some(BlockKind::Tool(id)) = blocks.get(&index) {
                                    let json = delta["partial_json"]
                                        .as_str()
                                        .unwrap_or_default()
                                        .to_owned();
                                    yield ProviderEvent::ToolCallDelta { id: id.clone(), json };
                                }
                            }
                            _ => {}
                        }
                    }
                    "content_block_stop" => {
                        let index = data["index"].as_u64().unwrap_or_default();
                        if let Some(BlockKind::Tool(id)) = blocks.remove(&index) {
                            yield ProviderEvent::ToolCallEnd { id };
                        }
                    }
                    "message_delta" => {
                        if let Some(reason) = data["delta"]["stop_reason"].as_str() {
                            stop = stop_reason(reason);
                        }
                        yield ProviderEvent::Usage(usage(&data["usage"]));
                    }
                    "message_stop" => {
                        yield ProviderEvent::Done { stop: stop.clone() };
                    }
                    "error" => {
                        let message =
                            data["error"]["message"].as_str().unwrap_or_default().to_owned();
                        Err(ProviderError::Api {
                            status: 0,
                            message,
                            retry_after: None,
                        })?;
                    }
                    _ => {}
                }
            }
        };
        Ok(Box::pin(stream))
    }
}

enum BlockKind {
    Tool(CallId),
    Other,
}

impl BlockKind {
    fn other(_kind: &str) -> Self {
        BlockKind::Other
    }
}

fn usage(value: &Value) -> TokenUsage {
    TokenUsage {
        input: value["input_tokens"].as_u64().unwrap_or_default(),
        output: value["output_tokens"].as_u64().unwrap_or_default(),
        cache_read: value["cache_read_input_tokens"]
            .as_u64()
            .unwrap_or_default(),
        cache_write: value["cache_creation_input_tokens"]
            .as_u64()
            .unwrap_or_default(),
    }
}

fn stop_reason(reason: &str) -> StopReason {
    match reason {
        "end_turn" | "stop_sequence" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        other => StopReason::Other(other.to_owned()),
    }
}
