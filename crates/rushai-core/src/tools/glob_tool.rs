use std::time::SystemTime;

use rushai_provider::ToolDef;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::resolve;
use crate::permission::PermissionSpec;
use crate::tool::{Tool, ToolCtx, ToolError, parse_input, schema_for};

const MAX_MATCHES: usize = 500;

#[derive(Deserialize, JsonSchema)]
struct Input {
    /// Glob pattern, e.g. `src/**/*.rs`.
    pattern: String,
    /// Base directory; defaults to the working directory.
    path: Option<String>,
}

pub struct GlobTool;

#[async_trait::async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &'static str {
        "glob"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "glob".into(),
            description: include_str!("descriptions/glob.md").into(),
            input_schema: schema_for::<Input>(),
        }
    }

    fn permission(&self, _input: &Value) -> Option<PermissionSpec> {
        None
    }

    async fn run(&self, ctx: &ToolCtx, input: Value) -> Result<String, ToolError> {
        let input: Input = parse_input(input)?;
        let base = resolve(&ctx.cwd, input.path.as_deref().unwrap_or("."));
        let full = base.join(&input.pattern);
        let pattern = full.to_string_lossy().into_owned();
        let paths =
            glob::glob(&pattern).map_err(|e| ToolError::Input(format!("bad pattern: {e}")))?;

        let mut matches: Vec<(SystemTime, String)> = Vec::new();
        for path in paths.flatten() {
            if ctx.cancel.is_cancelled() {
                return Err(ToolError::Canceled);
            }
            let mtime = path
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let shown = path
                .strip_prefix(&base)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| path.to_string_lossy().into_owned());
            matches.push((mtime, shown));
        }
        // Most recently modified first; ties stay deterministic by name.
        matches.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

        if matches.is_empty() {
            return Ok("no matches\n".into());
        }
        let truncated = matches.len() > MAX_MATCHES;
        let mut out = String::new();
        for (_, path) in matches.into_iter().take(MAX_MATCHES) {
            out.push_str(&path);
            out.push('\n');
        }
        if truncated {
            out.push_str("... matches truncated\n");
        }
        Ok(out)
    }
}
