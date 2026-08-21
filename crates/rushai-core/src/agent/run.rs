use std::sync::Arc;

use futures::StreamExt;
use rushai_protocol::{
    Event, FinishReason, MessageId, Part, PartDelta, Role, SessionId, TokenUsage, UserPart,
};
use rushai_provider::{ChatMessage, ChatRequest, EventStream, Provider, ProviderEvent, StopReason};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::permission::{PermissionError, PermissionService};
use crate::store::{Store, StoredMessage};
use crate::tool::{Tool, ToolCtx, ToolError, dispatch};

const MAX_TURNS: usize = 50;

pub(super) struct RunCtx {
    pub store: Store,
    pub provider: Arc<dyn Provider>,
    pub tools: Arc<[Arc<dyn Tool>]>,
    pub permissions: Arc<PermissionService>,
    pub events: mpsc::Sender<Event>,
    pub cancel: CancellationToken,
    pub session: SessionId,
    pub cwd: std::path::PathBuf,
    pub system: String,
    pub max_tokens: Option<u64>,
    pub prompt: Vec<UserPart>,
}

pub(super) fn system_prompt() -> String {
    include_str!("prompt.md")
        .replace("{platform}", std::env::consts::OS)
        .trim()
        .to_owned()
}

pub(super) async fn run(ctx: RunCtx) -> FinishReason {
    match drive(&ctx).await {
        Ok(reason) => reason,
        Err(RunError::Canceled) => FinishReason::Canceled,
        Err(RunError::Fatal(message)) => {
            let _ = ctx.events.send(Event::Error { message }).await;
            FinishReason::Error
        }
    }
}

enum RunError {
    Canceled,
    Fatal(String),
}

impl From<crate::store::StoreError> for RunError {
    fn from(err: crate::store::StoreError) -> Self {
        RunError::Fatal(format!("store error: {err}"))
    }
}

async fn drive(ctx: &RunCtx) -> Result<FinishReason, RunError> {
    let mut history = load_history(ctx).await?;
    let user = message(
        ctx,
        Role::User,
        ctx.prompt
            .iter()
            .map(|part| match part {
                UserPart::Text { text } => Part::Text { text: text.clone() },
                UserPart::File { path } => Part::File { path: path.clone() },
            })
            .collect(),
    );
    ctx.store.save_message(&user).await?;
    history.push(user);

    for _ in 0..MAX_TURNS {
        let request = ChatRequest {
            system: format!("{}\n\nWorking directory: {}", ctx.system, ctx.cwd.display()),
            messages: history
                .iter()
                .map(|m| ChatMessage {
                    role: m.role,
                    parts: m.parts.clone(),
                })
                .collect(),
            tools: ctx.tools.iter().map(|t| t.def()).collect(),
            max_tokens: ctx.max_tokens,
        };
        let stream = tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => return Err(RunError::Canceled),
            stream = ctx.provider.stream(&request) => {
                stream.map_err(|e| RunError::Fatal(e.to_string()))?
            }
        };

        let turn = consume(ctx, stream).await?;
        history.push(turn.message);
        match turn.stop {
            StopReason::ToolUse => {
                let results = execute_tools(ctx, &turn.calls).await?;
                history.push(results);
            }
            StopReason::EndTurn | StopReason::Other(_) => return Ok(FinishReason::EndTurn),
            StopReason::MaxTokens => return Ok(FinishReason::MaxTokens),
        }
    }
    Err(RunError::Fatal(format!(
        "run exceeded {MAX_TURNS} provider turns"
    )))
}

async fn load_history(ctx: &RunCtx) -> Result<Vec<StoredMessage>, RunError> {
    let messages = ctx.store.messages(&ctx.session).await?;
    let summary = ctx
        .store
        .session(&ctx.session)
        .await?
        .and_then(|s| s.summary_message_id);
    Ok(match summary {
        Some(id) => {
            let at = messages.iter().position(|m| m.id == id).unwrap_or(0);
            messages[at..].to_vec()
        }
        None => messages,
    })
}

fn message(ctx: &RunCtx, role: Role, parts: Vec<Part>) -> StoredMessage {
    StoredMessage {
        id: MessageId::new(),
        session: ctx.session.clone(),
        role,
        provider: None,
        model: Some(ctx.provider.model().id.clone()),
        parts,
        is_summary: false,
        created_at: now_ms(),
    }
}

struct Turn {
    message: StoredMessage,
    calls: Vec<Call>,
    stop: StopReason,
}

struct Call {
    id: rushai_protocol::CallId,
    name: String,
    input: Value,
}

/// What kind of part is currently streaming; index of the open part is
/// always `parts.len() - 1`.
enum Open {
    None,
    Text,
    Reasoning,
    ToolJson(String),
}

async fn consume(ctx: &RunCtx, mut stream: EventStream) -> Result<Turn, RunError> {
    let mut msg = message(ctx, Role::Assistant, Vec::new());
    let mut open = Open::None;
    let mut usage = TokenUsage::default();

    // Every exit path funnels through one finalize below, so the partial
    // message is persisted with a Finish part exactly once.
    let outcome = stream_turn(ctx, &mut stream, &mut msg, &mut open, &mut usage).await;
    let stop = match outcome {
        Ok(Some(stop)) => stop,
        Ok(None) => {
            finalize(ctx, &mut msg, FinishReason::Error, usage).await?;
            return Err(RunError::Fatal(
                "provider stream ended without a done event".into(),
            ));
        }
        Err(RunError::Canceled) => {
            finalize(ctx, &mut msg, FinishReason::Canceled, usage).await?;
            return Err(RunError::Canceled);
        }
        Err(err) => {
            finalize(ctx, &mut msg, FinishReason::Error, usage).await?;
            return Err(err);
        }
    };
    close_open(ctx, &mut msg, &mut open).await?;
    let reason = match &stop {
        StopReason::ToolUse => FinishReason::ToolUse,
        StopReason::MaxTokens => FinishReason::MaxTokens,
        StopReason::EndTurn | StopReason::Other(_) => FinishReason::EndTurn,
    };
    finalize(ctx, &mut msg, reason, usage).await?;

    let calls = msg
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::ToolCall { id, name, input } => Some(Call {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            }),
            _ => None,
        })
        .collect();
    Ok(Turn {
        message: msg,
        calls,
        stop,
    })
}

async fn stream_turn(
    ctx: &RunCtx,
    stream: &mut EventStream,
    msg: &mut StoredMessage,
    open: &mut Open,
    usage: &mut TokenUsage,
) -> Result<Option<StopReason>, RunError> {
    emit(
        ctx,
        Event::MessageStarted {
            message: msg.id.clone(),
            role: Role::Assistant,
        },
    )
    .await?;

    loop {
        let item = tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => return Err(RunError::Canceled),
            item = stream.next() => item,
        };
        let Some(item) = item else { return Ok(None) };
        let event = match item {
            Ok(event) => event,
            Err(err) => return Err(RunError::Fatal(err.to_string())),
        };
        match event {
            ProviderEvent::TextDelta(delta) => {
                if !matches!(open, Open::Text) {
                    close_open(ctx, msg, open).await?;
                    msg.parts.push(Part::Text {
                        text: String::new(),
                    });
                    *open = Open::Text;
                }
                if let Some(Part::Text { text }) = msg.parts.last_mut() {
                    text.push_str(&delta);
                }
                let index = msg.parts.len() - 1;
                emit(
                    ctx,
                    Event::PartDelta {
                        message: msg.id.clone(),
                        index,
                        delta: PartDelta::Text { delta },
                    },
                )
                .await?;
            }
            ProviderEvent::Reasoning { text, signature } => {
                if !matches!(open, Open::Reasoning) {
                    close_open(ctx, msg, open).await?;
                    msg.parts.push(Part::Reasoning {
                        text: String::new(),
                        signature: None,
                    });
                    *open = Open::Reasoning;
                }
                if let Some(Part::Reasoning {
                    text: buffer,
                    signature: sig,
                }) = msg.parts.last_mut()
                {
                    buffer.push_str(&text);
                    if signature.is_some() {
                        *sig = signature;
                    }
                }
                if !text.is_empty() {
                    let index = msg.parts.len() - 1;
                    emit(
                        ctx,
                        Event::PartDelta {
                            message: msg.id.clone(),
                            index,
                            delta: PartDelta::Reasoning { delta: text },
                        },
                    )
                    .await?;
                }
            }
            ProviderEvent::ToolCallStart { id, name } => {
                close_open(ctx, msg, open).await?;
                msg.parts.push(Part::ToolCall {
                    id,
                    name,
                    input: Value::Null,
                });
                *open = Open::ToolJson(String::new());
            }
            ProviderEvent::ToolCallDelta { json, .. } => {
                if let Open::ToolJson(buffer) = open {
                    buffer.push_str(&json);
                    let index = msg.parts.len() - 1;
                    emit(
                        ctx,
                        Event::PartDelta {
                            message: msg.id.clone(),
                            index,
                            delta: PartDelta::ToolInput { json },
                        },
                    )
                    .await?;
                }
            }
            ProviderEvent::ToolCallEnd { .. } => {
                close_open(ctx, msg, open).await?;
            }
            ProviderEvent::Usage(turn_usage) => {
                *usage = add(*usage, turn_usage);
                emit(
                    ctx,
                    Event::Usage {
                        session: ctx.session.clone(),
                        usage: turn_usage,
                    },
                )
                .await?;
            }
            ProviderEvent::Done { stop: reason } => {
                return Ok(Some(reason));
            }
        }
    }
}

/// Close the streaming part: parse buffered tool input, emit PartDone,
/// persist the message so far.
async fn close_open(
    ctx: &RunCtx,
    msg: &mut StoredMessage,
    open: &mut Open,
) -> Result<(), RunError> {
    let closing = std::mem::replace(open, Open::None);
    if matches!(closing, Open::None) {
        return Ok(());
    }
    if let Open::ToolJson(buffer) = closing
        && let Some(Part::ToolCall { input, .. }) = msg.parts.last_mut()
    {
        *input = if buffer.trim().is_empty() {
            Value::Object(serde_json::Map::new())
        } else {
            serde_json::from_str(&buffer).unwrap_or(Value::String(buffer))
        };
    }
    let index = msg.parts.len() - 1;
    let part = msg.parts[index].clone();
    ctx.store.save_message(msg).await?;
    emit(
        ctx,
        Event::PartDone {
            message: msg.id.clone(),
            index,
            part,
        },
    )
    .await
}

/// Append the finish part and persist. Runs on every exit path, including
/// cancellation, so history stays coherent.
async fn finalize(
    ctx: &RunCtx,
    msg: &mut StoredMessage,
    reason: FinishReason,
    usage: TokenUsage,
) -> Result<(), RunError> {
    if let Some(Part::ToolCall { input, .. }) = msg.parts.last_mut()
        && input.is_null()
    {
        *input = Value::Object(serde_json::Map::new());
    }
    msg.parts.push(Part::Finish { reason, usage });
    ctx.store.save_message(msg).await?;
    let _ = ctx
        .events
        .send(Event::PartDone {
            message: msg.id.clone(),
            index: msg.parts.len() - 1,
            part: Part::Finish { reason, usage },
        })
        .await;
    Ok(())
}

/// Run every tool call from the turn. On cancellation the remaining calls
/// still produce error results, so each tool_use has a matching result when
/// the history replays.
async fn execute_tools(ctx: &RunCtx, calls: &[Call]) -> Result<StoredMessage, RunError> {
    let tool_ctx = ToolCtx {
        session: ctx.session.clone(),
        cwd: ctx.cwd.clone(),
        cancel: ctx.cancel.clone(),
        permissions: ctx.permissions.clone(),
    };
    let mut parts = Vec::with_capacity(calls.len());
    let mut canceled = ctx.cancel.is_cancelled();
    for call in calls {
        let _ = ctx
            .events
            .send(Event::ToolStarted {
                call: call.id.clone(),
                name: call.name.clone(),
                input: call.input.clone(),
            })
            .await;
        let outcome = if canceled {
            Err(ToolError::Canceled)
        } else {
            match ctx.tools.iter().find(|t| t.name() == call.name) {
                None => Err(ToolError::Input(format!("no such tool: {}", call.name))),
                Some(tool) => tokio::select! {
                    biased;
                    _ = ctx.cancel.cancelled() => {
                        canceled = true;
                        Err(ToolError::Canceled)
                    }
                    outcome = dispatch(tool.as_ref(), &tool_ctx, call.input.clone()) => outcome,
                },
            }
        };
        let (content, is_error) = match outcome {
            Ok(output) => (output, false),
            Err(ToolError::Permission(PermissionError::Denied(what))) => {
                (format!("permission denied: {what}"), true)
            }
            Err(err) => (err.to_string(), true),
        };
        let _ = ctx
            .events
            .send(Event::ToolDone {
                call: call.id.clone(),
                output: content.clone(),
                is_error,
            })
            .await;
        parts.push(Part::ToolResult {
            id: call.id.clone(),
            content,
            is_error,
        });
    }
    let results = message(ctx, Role::User, parts);
    ctx.store.save_message(&results).await?;
    if canceled {
        return Err(RunError::Canceled);
    }
    Ok(results)
}

async fn emit(ctx: &RunCtx, event: Event) -> Result<(), RunError> {
    tokio::select! {
        biased;
        _ = ctx.cancel.cancelled() => Err(RunError::Canceled),
        sent = ctx.events.send(event) => {
            sent.map_err(|_| RunError::Fatal("event channel closed".into()))
        }
    }
}

fn add(a: TokenUsage, b: TokenUsage) -> TokenUsage {
    TokenUsage {
        input: a.input + b.input,
        output: a.output + b.output,
        cache_read: a.cache_read + b.cache_read,
        cache_write: a.cache_write + b.cache_write,
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as i64
}
