use serde::{Deserialize, Serialize};

use crate::ids::{RequestId, SessionId};
use crate::parts::UserPart;
use crate::permission::Decision;

/// A request submitted by a frontend to the engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Op {
    Prompt {
        session: SessionId,
        parts: Vec<UserPart>,
    },
    Cancel {
        session: SessionId,
    },
    PermissionDecision {
        request: RequestId,
        decision: Decision,
    },
    Compact {
        session: SessionId,
    },
    Shutdown,
}
