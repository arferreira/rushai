use rushai_provider::ToolDef;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::edits::{EditSpec, LineEndings, apply_one, detect_endings, to_crlf, to_lf};
use super::{atomic_write, missing, real_target, resolve, resolve_write};
use crate::permission::PermissionSpec;
use crate::tool::{RunToken, Tool, ToolCtx, ToolError, parse_input, schema_for};

#[derive(Deserialize, JsonSchema)]
struct Input {
    /// File to edit.
    path: String,
    /// Edits applied in order; each matches the result of the previous ones.
    edits: Vec<EditSpec>,
}

pub struct MultiEdit;

#[async_trait::async_trait]
impl Tool for MultiEdit {
    fn name(&self) -> &'static str {
        "multiedit"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "multiedit".into(),
            description: include_str!("descriptions/multiedit.md").into(),
            input_schema: schema_for::<Input>(),
        }
    }

    fn permission(&self, ctx: &ToolCtx, input: &Value) -> Option<PermissionSpec> {
        let path = input
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let count = input
            .get("edits")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let target = real_target(&ctx.cwd, path);
        Some(PermissionSpec::new(
            "multiedit",
            "write",
            Some(path.to_owned()),
            format!("apply {count} edits to {}", target.display()),
        ))
    }

    async fn run(&self, ctx: &ToolCtx, input: Value, _run: RunToken) -> Result<String, ToolError> {
        let input: Input = parse_input(input)?;
        if input.edits.is_empty() {
            return Err(ToolError::Input("no edits given".into()));
        }
        let path = resolve(&ctx.cwd, &input.path);
        if !path.is_file() {
            return Err(missing(&input.path));
        }
        resolve_write(&ctx.cwd, &input.path)?;

        let original = tokio::fs::read_to_string(&path).await?;
        let endings = detect_endings(&original)?;

        // Validate and apply every edit to an in-memory copy first. Nothing
        // reaches disk unless the whole sequence succeeds, and the write
        // itself is atomic, so a failure never leaves a half-edited file.
        let mut content = to_lf(&original);
        for (i, edit) in input.edits.iter().enumerate() {
            content = apply_one(&content, edit)
                .map_err(|e| ToolError::Failed(format!("edit {}: {e}", i + 1)))?;
        }

        let out = match endings {
            LineEndings::Crlf => to_crlf(&content),
            LineEndings::Lf => content,
        };
        atomic_write(&path, out.as_bytes()).await?;
        Ok(format!(
            "applied {} edits to {}",
            input.edits.len(),
            input.path
        ))
    }
}
