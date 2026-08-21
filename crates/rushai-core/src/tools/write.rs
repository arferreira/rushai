use rushai_provider::ToolDef;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::{atomic_write, real_target, resolve_write};
use crate::permission::PermissionSpec;
use crate::tool::{RunToken, Tool, ToolCtx, ToolError, parse_input, schema_for};

#[derive(Deserialize, JsonSchema)]
struct Input {
    /// File to write.
    path: String,
    /// Full contents to write.
    content: String,
}

pub struct Write;

#[async_trait::async_trait]
impl Tool for Write {
    fn name(&self) -> &'static str {
        "write"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "write".into(),
            description: include_str!("descriptions/write.md").into(),
            input_schema: schema_for::<Input>(),
        }
    }

    fn permission(&self, ctx: &ToolCtx, input: &Value) -> Option<PermissionSpec> {
        let path = input
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        // Resolve against the tool's cwd, not the process cwd, and show the
        // real target (symlinks followed) so the human sees where bytes land.
        let target = real_target(&ctx.cwd, path);
        let verb = if target.exists() {
            "overwrite"
        } else {
            "create"
        };
        Some(PermissionSpec::new(
            "write",
            "write",
            Some(path.to_owned()),
            format!("{verb} {}", target.display()),
        ))
    }

    async fn run(&self, ctx: &ToolCtx, input: Value, _run: RunToken) -> Result<String, ToolError> {
        let input: Input = parse_input(input)?;
        let path = resolve_write(&ctx.cwd, &input.path)?;
        atomic_write(&path, input.content.as_bytes()).await?;
        Ok(format!(
            "wrote {} bytes to {}",
            input.content.len(),
            input.path
        ))
    }
}
