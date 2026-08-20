use std::collections::BTreeMap;

use async_stream::try_stream;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use rushai_protocol::{CallId, Part, Role, TokenUsage};
use serde_json::{Value, json};

use crate::{
    ChatRequest, EventStream, ModelInfo, Provider, ProviderError, ProviderEvent, StopReason,
};

/// Client for OpenAI's chat completions protocol and everything that
/// imitates it (OpenRouter, Groq, Ollama, local servers).
pub struct OpenAiCompat {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: ModelInfo,
}

impl OpenAiCompat {
    pub fn new(api_key: String, model: ModelInfo, base_url: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url,
            api_key,
            model,
        }
    }

    fn body(&self, request: &ChatRequest) -> Value {
        let mut messages: Vec<Value> = Vec::new();
        if !request.system.is_empty() {
            messages.push(json!({ "role": "system", "content": request.system }));
        }
        for message in &request.messages {
            append_message(&mut messages, message.role, &message.parts);
        }

        let mut body = json!({
            "model": self.model.id,
            "stream": true,
            "stream_options": { "include_usage": true },
            "messages": messages,
        });
        if let Some(max) = request.max_tokens {
            body["max_completion_tokens"] = json!(max);
        }
        if !request.tools.is_empty() {
            body["tools"] = Value::Array(
                request
                    .tools
                    .iter()
                    .map(|tool| {
                        json!({
                            "type": "function",
                            "function": {
                                "name": tool.name,
                                "description": tool.description,
                                "parameters": tool.input_schema,
                            },
                        })
                    })
                    .collect(),
            );
        }
        body
    }
}

/// One parts-based message can fan out: tool results become their own
/// `role: tool` wire messages.
fn append_message(messages: &mut Vec<Value>, role: Role, parts: &[Part]) {
    let mut text = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    for part in parts {
        match part {
            Part::Text { text: t } => text.push_str(t),
            Part::ToolCall { id, name, input } => tool_calls.push(json!({
                "id": id.as_str(),
                "type": "function",
                "function": { "name": name, "arguments": input.to_string() },
            })),
            Part::ToolResult {
                id,
                content,
                is_error: _,
            } => messages.push(json!({
                "role": "tool",
                "tool_call_id": id.as_str(),
                "content": content,
            })),
            // The chat completions protocol has no reasoning replay.
            Part::Reasoning { .. } | Part::File { .. } | Part::Finish { .. } => {}
        }
    }
    if text.is_empty() && tool_calls.is_empty() {
        return;
    }
    let mut message = json!({
        "role": match role {
            Role::User => "user",
            Role::Assistant => "assistant",
        },
        "content": text,
    });
    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }
    messages.push(message);
}

#[async_trait::async_trait]
impl Provider for OpenAiCompat {
    fn model(&self) -> &ModelInfo {
        &self.model
    }

    async fn stream(&self, request: ChatRequest) -> Result<EventStream, ProviderError> {
        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&self.body(&request))
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            return Err(ProviderError::Api {
                status: status.as_u16(),
                message,
            });
        }

        let mut sse = response.bytes_stream().eventsource();
        let stream = try_stream! {
            // Tool call deltas arrive keyed by index; ids only on the first.
            let mut calls: BTreeMap<u64, CallId> = BTreeMap::new();
            let mut stop = StopReason::EndTurn;
            while let Some(event) = sse.next().await {
                let event = event.map_err(|e| ProviderError::Stream(e.to_string()))?;
                if event.data.trim() == "[DONE]" {
                    yield ProviderEvent::Done { stop: stop.clone() };
                    break;
                }
                let data: Value = serde_json::from_str(&event.data)
                    .map_err(|e| ProviderError::Protocol(format!("bad chunk json: {e}")))?;
                if let Some(error) = data.get("error") {
                    let message = error["message"].as_str().unwrap_or_default().to_owned();
                    Err(ProviderError::Api { status: 0, message })?;
                }
                if let Some(usage) = data.get("usage").filter(|u| !u.is_null()) {
                    yield ProviderEvent::Usage(TokenUsage {
                        input: usage["prompt_tokens"].as_u64().unwrap_or_default(),
                        output: usage["completion_tokens"].as_u64().unwrap_or_default(),
                        cache_read: usage["prompt_tokens_details"]["cached_tokens"]
                            .as_u64()
                            .unwrap_or_default(),
                        cache_write: 0,
                    });
                }
                let Some(choice) = data["choices"].get(0) else {
                    continue;
                };
                let delta = &choice["delta"];
                if let Some(text) = delta["content"].as_str().filter(|t| !t.is_empty()) {
                    yield ProviderEvent::TextDelta(text.to_owned());
                }
                for key in ["reasoning_content", "reasoning"] {
                    if let Some(text) = delta[key].as_str().filter(|t| !t.is_empty()) {
                        yield ProviderEvent::Reasoning {
                            text: text.to_owned(),
                            signature: None,
                        };
                    }
                }
                if let Some(deltas) = delta["tool_calls"].as_array() {
                    for tc in deltas {
                        let index = tc["index"].as_u64().unwrap_or_default();
                        if let Some(id) = tc["id"].as_str() {
                            let id = CallId::from(id);
                            calls.insert(index, id.clone());
                            let name =
                                tc["function"]["name"].as_str().unwrap_or_default().to_owned();
                            yield ProviderEvent::ToolCallStart { id, name };
                        }
                        if let Some(arguments) = tc["function"]["arguments"]
                            .as_str()
                            .filter(|a| !a.is_empty())
                            && let Some(id) = calls.get(&index)
                        {
                            yield ProviderEvent::ToolCallDelta {
                                id: id.clone(),
                                json: arguments.to_owned(),
                            };
                        }
                    }
                }
                if let Some(reason) = choice["finish_reason"].as_str() {
                    stop = match reason {
                        "stop" => StopReason::EndTurn,
                        "tool_calls" => StopReason::ToolUse,
                        "length" => StopReason::MaxTokens,
                        other => StopReason::Other(other.to_owned()),
                    };
                    for id in std::mem::take(&mut calls).into_values() {
                        yield ProviderEvent::ToolCallEnd { id };
                    }
                }
            }
        };
        Ok(Box::pin(stream))
    }
}
