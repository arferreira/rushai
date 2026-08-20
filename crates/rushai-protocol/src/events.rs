use serde::{Deserialize, Serialize};

use crate::ids::{CallId, MessageId, SessionId};
use crate::parts::{FinishReason, Part, PartDelta, Role};
use crate::permission::PermissionRequest;
use crate::usage::TokenUsage;

/// A state change emitted by the engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A prompt was accepted and stamped with a monotonic sequence number.
    /// Cancellation reasons about these sequence numbers.
    RunAccepted {
        session: SessionId,
        seq: u64,
    },
    MessageStarted {
        message: MessageId,
        role: Role,
    },
    PartDelta {
        message: MessageId,
        index: usize,
        delta: PartDelta,
    },
    PartDone {
        message: MessageId,
        index: usize,
        part: Part,
    },
    ToolStarted {
        call: CallId,
        name: String,
        input: serde_json::Value,
    },
    ToolDone {
        call: CallId,
        output: String,
        is_error: bool,
    },
    PermissionRequested {
        request: PermissionRequest,
    },
    Usage {
        session: SessionId,
        usage: TokenUsage,
    },
    RunFinished {
        session: SessionId,
        seq: u64,
        reason: FinishReason,
    },
    Error {
        message: String,
    },
}
