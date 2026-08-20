use rushai_provider::ToolDef;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::{missing, resolve};
use crate::permission::PermissionSpec;
use crate::tool::{Tool, ToolCtx, ToolError, parse_input, schema_for};

const MAX_LINES: usize = 2000;
const MAX_BYTES: usize = 256 * 1024;

#[derive(Deserialize, JsonSchema)]
struct Input {
    /// File to read.
    path: String,
    /// 1-based line to start from.
    offset: Option<usize>,
    /// Maximum number of lines.
    limit: Option<usize>,
}

pub struct View;

#[async_trait::async_trait]
impl Tool for View {
    fn name(&self) -> &'static str {
        "view"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "view".into(),
            description: include_str!("descriptions/view.md").into(),
            input_schema: schema_for::<Input>(),
        }
    }

    fn permission(&self, _input: &Value) -> Option<PermissionSpec> {
        None
    }

    async fn run(&self, ctx: &ToolCtx, input: Value) -> Result<String, ToolError> {
        let input: Input = parse_input(input)?;
        let path = resolve(&ctx.cwd, &input.path);
        if !path.is_file() {
            return Err(missing(&input.path));
        }
        let bytes = tokio::fs::read(&path).await?;
        if bytes[..bytes.len().min(8192)].contains(&0) {
            return Err(ToolError::Failed(format!(
                "{} is a binary file",
                input.path
            )));
        }
        let text = String::from_utf8_lossy(&bytes);

        let offset = input.offset.unwrap_or(1).max(1);
        let limit = input.limit.unwrap_or(MAX_LINES).min(MAX_LINES);
        let mut out = String::new();
        let mut shown = 0usize;
        let mut truncated = false;
        for (n, line) in text.lines().enumerate().skip(offset - 1) {
            if shown == limit || out.len() + line.len() > MAX_BYTES {
                truncated = true;
                break;
            }
            out.push_str(&format!("{:>5}\t{}\n", n + 1, line));
            shown += 1;
        }
        if shown == 0 && !truncated {
            return Ok(format!("{} is empty past line {offset}", input.path));
        }
        if truncated {
            out.push_str("... output truncated; use offset/limit to read more\n");
        }
        Ok(out)
    }
}
