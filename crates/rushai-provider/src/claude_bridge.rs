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
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
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
    pub fn discover(model: ModelInfo) -> Self {
        let bin = match std::env::var("CLAUDE_BIN") {
            Ok(bin) if !bin.is_empty() => PathBuf::from(bin),
            _ => PathBuf::from("claude"),
        };
        Self::new(bin, model)
    }

    /// The CLI takes one prompt string, so prior turns are serialized with
    /// `User:` / `Assistant:` role markers. Known limitations: a message
    /// body containing `\n\nAssistant:\n` can forge a turn boundary, and a
    /// turn whose parts are all non-text (tool calls, files) serializes to
    /// nothing. Both are inherent to flattening history into one prompt;
    /// the bridge is chat-only, so real tool traffic never reaches it.
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
        // The prompt goes through stdin, never argv: argv would let a
        // prompt starting with "-" parse as CLI flags, and caps out at
        // ARG_MAX long before a real transcript does. `=` keeps the
        // system prompt a single unambiguous token.
        command
            .arg("-p")
            .args(["--output-format", "stream-json", "--verbose"])
            .args(["--model", &self.model.id])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if !request.system.is_empty() {
            command.arg(format!("--append-system-prompt={}", request.system));
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

        let mut stdin = child.stdin.take().expect("stdin piped");
        let prompt = Self::prompt(request);
        tokio::spawn(async move {
            let _ = stdin.write_all(prompt.as_bytes()).await;
            // Dropping stdin closes the pipe so the CLI sees EOF.
        });

        let stdout = child.stdout.take().expect("stdout piped");
        let mut stderr = child.stderr.take().expect("stderr piped");
        // Drain stderr concurrently so a chatty CLI can't fill the pipe
        // and deadlock the child.
        let mut stderr_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf).await;
            String::from_utf8_lossy(&buf).into_owned()
        });

        let stream = try_stream! {
            let mut lines = BufReader::new(stdout).lines();
            let mut finished = false;
            let mut failure: Option<String> = None;
            while let Some(line) = lines
                .next_line()
                .await
                .map_err(|e| ProviderError::Stream(e.to_string()))?
            {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(event) = serde_json::from_str::<Value>(&line) else {
                    // Runtimes love printing warnings around the JSON.
                    tracing::warn!(line, "skipping unparseable claude CLI output");
                    continue;
                };
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
                        } else {
                            let subtype =
                                event["subtype"].as_str().unwrap_or("unknown").to_owned();
                            failure = Some(format!("claude CLI result: {subtype}"));
                        }
                        break;
                    }
                    _ => {}
                }
            }

            // Reap the child and collect stderr before reporting anything,
            // success included, so no path leaves a zombie or loses the
            // diagnostic.
            let status = child
                .wait()
                .await
                .map_err(|e| ProviderError::Stream(e.to_string()))?;
            let stderr = (&mut stderr_task).await.unwrap_or_default();
            if let Some(message) = failure {
                Err(ProviderError::Stream(with_stderr(message, &stderr)))?;
            }
            if !status.success() {
                Err(ProviderError::Stream(with_stderr(
                    format!("claude CLI exited with {status}"),
                    &stderr,
                )))?;
            }
            if !finished {
                Err(ProviderError::Stream(with_stderr(
                    "claude CLI stream ended without a result event".into(),
                    &stderr,
                )))?;
            }
            yield ProviderEvent::Done { stop: StopReason::EndTurn };
        };
        Ok(Box::pin(stream))
    }
}

fn with_stderr(message: String, stderr: &str) -> String {
    let stderr = stderr.trim();
    if stderr.is_empty() {
        return message;
    }
    let skip = stderr.chars().count().saturating_sub(500);
    let tail: String = stderr.chars().skip(skip).collect();
    format!("{message}: {tail}")
}
