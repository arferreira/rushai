use rushai_provider::ToolDef;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::{missing, resolve};
use crate::permission::PermissionSpec;
use crate::tool::{RunToken, Tool, ToolCtx, ToolError, parse_input, schema_for};

const MAX_ENTRIES: usize = 1000;

#[derive(Deserialize, JsonSchema)]
struct Input {
    /// Directory to list; defaults to the working directory.
    path: Option<String>,
}

pub struct Ls;

#[async_trait::async_trait]
impl Tool for Ls {
    fn name(&self) -> &'static str {
        "ls"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "ls".into(),
            description: include_str!("descriptions/ls.md").into(),
            input_schema: schema_for::<Input>(),
        }
    }

    fn permission(&self, _input: &Value) -> Option<PermissionSpec> {
        None
    }

    async fn run(&self, ctx: &ToolCtx, input: Value, _run: RunToken) -> Result<String, ToolError> {
        let input: Input = parse_input(input)?;
        let shown = input.path.as_deref().unwrap_or(".");
        let path = resolve(&ctx.cwd, shown);
        if !path.is_dir() {
            return Err(missing(shown));
        }
        let mut entries: Vec<String> = Vec::new();
        let mut dir = tokio::fs::read_dir(&path).await?;
        while let Some(entry) = dir.next_entry().await? {
            if ctx.cancel.is_cancelled() {
                return Err(ToolError::Canceled);
            }
            let mut name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().await?.is_dir() {
                name.push('/');
            }
            entries.push(name);
        }
        entries.sort();
        let mut out = String::new();
        let truncated = entries.len() > MAX_ENTRIES;
        for name in entries.iter().take(MAX_ENTRIES) {
            out.push_str(name);
            out.push('\n');
        }
        if truncated {
            out.push_str("... listing truncated\n");
        }
        if out.is_empty() {
            out.push_str("empty directory\n");
        }
        Ok(out)
    }
}
