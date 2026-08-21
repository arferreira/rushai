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
    #[serde(flatten)]
    edit: EditSpec,
}

pub struct Edit;

#[async_trait::async_trait]
impl Tool for Edit {
    fn name(&self) -> &'static str {
        "edit"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "edit".into(),
            description: include_str!("descriptions/edit.md").into(),
            input_schema: schema_for::<Input>(),
        }
    }

    fn permission(&self, ctx: &ToolCtx, input: &Value) -> Option<PermissionSpec> {
        let path = input
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let target = real_target(&ctx.cwd, path);
        Some(PermissionSpec::new(
            "edit",
            "write",
            Some(path.to_owned()),
            format!("edit {}", target.display()),
        ))
    }

    async fn run(&self, ctx: &ToolCtx, input: Value, _run: RunToken) -> Result<String, ToolError> {
        let input: Input = parse_input(input)?;
        let path = resolve(&ctx.cwd, &input.path);
        if !path.is_file() {
            return Err(missing(&input.path));
        }
        // Enforce containment before touching disk (returns the same path).
        resolve_write(&ctx.cwd, &input.path)?;

        let original = tokio::fs::read_to_string(&path).await?;
        let endings = detect_endings(&original)?;
        let normalized = to_lf(&original);
        let edited = apply_one(&normalized, &input.edit)?;
        let out = match endings {
            LineEndings::Crlf => to_crlf(&edited),
            LineEndings::Lf => edited,
        };
        atomic_write(&path, out.as_bytes()).await?;
        Ok(format!("edited {}", input.path))
    }
}
