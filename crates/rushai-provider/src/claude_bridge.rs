//! Claude subscription bridge.
//!
//! Shells out to the locally installed `claude` CLI, which is the
//! sanctioned way to draw on a Claude Pro/Max plan. Chat only: rush-side
//! tools are rejected because the CLI owns its own tool loop.

use std::path::PathBuf;
use std::process::Stdio;

use async_stream::try_stream;
use rushai_protocol::{Part, Role, TokenUsage};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

use crate::{
    ChatRequest, EventStream, ModelInfo, Provider, ProviderError, ProviderEvent, StopReason,
};

pub struct ClaudeBridge {
    bin: PathBuf,
    model: ModelInfo,
}

impl ClaudeBridge {
    pub fn new(bin: PathBuf, model: ModelInfo) -> Self {
        Self { bin, model }
    }

    /// `CLAUDE_BIN` override, else `claude` on PATH.
    pub fn discover(model: ModelInfo) -> Result<Self, ProviderError> {
        let bin = match std::env::var("CLAUDE_BIN") {
            Ok(bin) if !bin.is_empty() => PathBuf::from(bin),
            _ => PathBuf::from("claude"),
        };
        Ok(Self::new(bin, model))
    }

    /// The CLI takes one prompt string, so prior turns are serialized
    /// with role markers. Lossy for tool parts; fine for chat.
    fn prompt(request: &ChatRequest) -> String {
        let mut turns: Vec<(Role, String)> = Vec::new();
        for message in &request.messages {
            let text: String = message
                .parts
                .iter()
                .filter_map(|part| match part {
                    Part::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() {
                turns.push((message.role, text));
            }
        }
        if let [(Role::User, only)] = turns.as_slice() {
            return only.clone();
        }
        turns
            .iter()
            .map(|(role, text)| match role {
                Role::User => format!("User:\n{text}"),
                Role::Assistant => format!("Assistant:\n{text}"),
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

#[async_trait::async_trait]
impl Provider for ClaudeBridge {
    fn model(&self) -> &ModelInfo {
        &self.model
    }

    async fn stream(&self, request: &ChatRequest) -> Result<EventStream, ProviderError> {
        if !request.tools.is_empty() {
            return Err(ProviderError::Protocol(
                "the claude bridge is chat-only for now: rush-side tools are not supported".into(),
            ));
        }

        let mut command = Command::new(&self.bin);
        command
            .arg("-p")
            .arg(Self::prompt(request))
            .args(["--output-format", "stream-json", "--verbose"])
            .args(["--model", &self.model.id])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if !request.system.is_empty() {
            command.arg("--append-system-prompt").arg(&request.system);
        }

        let mut child = command.spawn().map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                ProviderError::Stream(format!(
                    "claude CLI not found at {:?}: install Claude Code or use an API key provider",
                    self.bin
                ))
            } else {
                ProviderError::Stream(format!("failed to start claude CLI: {err}"))
            }
        })?;

        let stdout = child.stdout.take().expect("stdout piped");
        let mut stderr = child.stderr.take().expect("stderr piped");
        // Drain stderr concurrently so a chatty CLI can't fill the pipe
        // and deadlock the child.
        let stderr_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf).await;
            String::from_utf8_lossy(&buf).into_owned()
        });

        let stream = try_stream! {
            let mut lines = BufReader::new(stdout).lines();
            let mut finished = false;
            while let Some(line) = lines
                .next_line()
                .await
                .map_err(|e| ProviderError::Stream(e.to_string()))?
            {
                if line.trim().is_empty() {
                    continue;
                }
                let event: Value = serde_json::from_str(&line)
                    .map_err(|e| ProviderError::Protocol(format!("bad stream-json line: {e}")))?;
                match event["type"].as_str().unwrap_or_default() {
                    "assistant" => {
                        if let Some(blocks) = event["message"]["content"].as_array() {
                            for block in blocks {
                                if block["type"] == "text"
                                    && let Some(text) = block["text"].as_str()
                                {
                                    yield ProviderEvent::TextDelta(text.to_owned());
                                }
                            }
                        }
                    }
                    "result" => {
                        let usage = &event["usage"];
                        yield ProviderEvent::Usage(TokenUsage {
                            input: usage["input_tokens"].as_u64().unwrap_or_default(),
                            output: usage["output_tokens"].as_u64().unwrap_or_default(),
                            cache_read: usage["cache_read_input_tokens"]
                                .as_u64()
                                .unwrap_or_default(),
                            cache_write: usage["cache_creation_input_tokens"]
                                .as_u64()
                                .unwrap_or_default(),
                        });
                        if event["subtype"] == "success" {
                            finished = true;
                            yield ProviderEvent::Done { stop: StopReason::EndTurn };
                        } else {
                            let detail = event["subtype"].as_str().unwrap_or("unknown").to_owned();
                            Err(ProviderError::Stream(format!("claude CLI result: {detail}")))?;
                        }
                    }
                    _ => {}
                }
            }
            let status = child
                .wait()
                .await
                .map_err(|e| ProviderError::Stream(e.to_string()))?;
            if !status.success() {
                let stderr = stderr_task.await.unwrap_or_default();
                let tail: String = stderr.chars().rev().take(500).collect::<Vec<_>>()
                    .into_iter().rev().collect();
                Err(ProviderError::Stream(format!(
                    "claude CLI exited with {status}: {}",
                    tail.trim()
                )))?;
            }
            if !finished {
                Err(ProviderError::Stream(
                    "claude CLI stream ended without a result event".into(),
                ))?;
            }
        };
        Ok(Box::pin(stream))
    }
}
