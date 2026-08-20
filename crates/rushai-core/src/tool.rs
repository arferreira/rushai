use std::path::PathBuf;
use std::sync::Arc;

use rushai_protocol::SessionId;
use rushai_provider::ToolDef;
use serde_json::Value;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::permission::{PermissionError, PermissionService, PermissionSpec};

pub struct ToolCtx {
    pub session: SessionId,
    pub cwd: PathBuf,
    pub cancel: CancellationToken,
    pub permissions: Arc<PermissionService>,
}

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("invalid input: {0}")]
    Input(String),
    #[error(transparent)]
    Permission(#[from] PermissionError),
    #[error("{0}")]
    Failed(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("canceled")]
    Canceled,
}

#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn def(&self) -> ToolDef;
    /// None means the tool needs no grant (read-only tools).
    fn permission(&self, input: &Value) -> Option<PermissionSpec>;
    async fn run(&self, ctx: &ToolCtx, input: Value) -> Result<String, ToolError>;
}

/// The only path from a tool call to execution: permission gate, then run.
pub async fn dispatch(tool: &dyn Tool, ctx: &ToolCtx, input: Value) -> Result<String, ToolError> {
    if let Some(spec) = tool.permission(&input) {
        ctx.permissions.ensure(&ctx.session, &spec).await?;
    }
    tool.run(ctx, input).await
}

pub(crate) fn parse_input<T: serde::de::DeserializeOwned>(input: Value) -> Result<T, ToolError> {
    serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))
}

pub(crate) fn schema_for<T: schemars::JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("schema serializes")
}
