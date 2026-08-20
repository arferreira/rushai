use serde::{Deserialize, Serialize};

use crate::ids::{RequestId, SessionId};

/// A tool asking to perform an action it has no grant for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub id: RequestId,
    pub session: SessionId,
    pub tool: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Allow this request only.
    Once,
    /// Allow for the rest of the session.
    Session,
    /// Allow and persist across sessions.
    Always,
    Deny,
}
