use std::path::Path;
use std::process::Stdio;

use rushai_provider::ToolDef;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::{missing, resolve};
use crate::permission::PermissionSpec;
use crate::tool::{Tool, ToolCtx, ToolError, parse_input, schema_for};

const MAX_MATCHES: usize = 200;
const MAX_BYTES: usize = 100 * 1024;

#[derive(Deserialize, JsonSchema)]
struct Input {
    /// Regular expression to search for.
    pattern: String,
    /// File or directory to search; defaults to the working directory.
    path: Option<String>,
    /// Glob restricting which files are searched, e.g. `*.rs`.
    include: Option<String>,
}

pub struct Grep;

#[async_trait::async_trait]
impl Tool for Grep {
    fn name(&self) -> &'static str {
        "grep"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "grep".into(),
            description: include_str!("descriptions/grep.md").into(),
            input_schema: schema_for::<Input>(),
        }
    }

    fn permission(&self, _input: &Value) -> Option<PermissionSpec> {
        None
    }

    async fn run(&self, ctx: &ToolCtx, input: Value) -> Result<String, ToolError> {
        let input: Input = parse_input(input)?;
        let shown = input.path.as_deref().unwrap_or(".");
        let target = resolve(&ctx.cwd, shown);
        if !target.exists() {
            return Err(missing(shown));
        }
        let raw = match rg(&input, &target, ctx).await {
            Some(result) => result?,
            None => walk(&input, &target, ctx)?,
        };
        Ok(cap(raw))
    }
}

/// `path:line:content` lines, or None when rg is not installed.
async fn rg(input: &Input, target: &Path, ctx: &ToolCtx) -> Option<Result<String, ToolError>> {
    let mut cmd = tokio::process::Command::new("rg");
    cmd.arg("--line-number")
        .arg("--no-heading")
        .arg("--color=never")
        .arg("--max-count=50");
    if let Some(include) = &input.include {
        cmd.arg("-g").arg(include);
    }
    cmd.arg("-e").arg(&input.pattern).arg(target);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(_) => return None,
    };
    let stdout = child.stdout.take().expect("piped stdout");
    let read = async {
        let mut out = String::new();
        use tokio::io::AsyncReadExt;
        let mut reader = tokio::io::BufReader::new(stdout).take((MAX_BYTES * 2) as u64);
        reader.read_to_string(&mut out).await?;
        let status = child.wait().await?;
        // rg exits 1 on no matches; only 2+ is an error.
        if status.code() == Some(2) {
            return Err(ToolError::Failed("rg failed".into()));
        }
        Ok(out)
    };
    let result = tokio::select! {
        result = read => result,
        _ = ctx.cancel.cancelled() => Err(ToolError::Canceled),
    };
    Some(result)
}

/// Fallback without rg: regex walk producing the same `path:line:content` shape.
fn walk(input: &Input, target: &Path, ctx: &ToolCtx) -> Result<String, ToolError> {
    let re = regex::Regex::new(&input.pattern)
        .map_err(|e| ToolError::Input(format!("bad pattern: {e}")))?;
    let include = input
        .include
        .as_deref()
        .map(glob::Pattern::new)
        .transpose()
        .map_err(|e| ToolError::Input(format!("bad include: {e}")))?;
    let mut out = String::new();
    let mut stack = vec![target.to_path_buf()];
    while let Some(path) = stack.pop() {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Canceled);
        }
        if path.is_dir() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name == ".git" || name == "target" || name == "node_modules" {
                continue;
            }
            for entry in std::fs::read_dir(&path)?.flatten() {
                stack.push(entry.path());
            }
            continue;
        }
        if let Some(pattern) = &include {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if !pattern.matches(&name) {
                continue;
            }
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if bytes[..bytes.len().min(8192)].contains(&0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        for (n, line) in text.lines().enumerate() {
            if re.is_match(line) {
                out.push_str(&format!("{}:{}:{}\n", path.display(), n + 1, line));
            }
            if out.len() > MAX_BYTES * 2 {
                return Ok(out);
            }
        }
    }
    Ok(out)
}

fn cap(raw: String) -> String {
    if raw.trim().is_empty() {
        return "no matches\n".into();
    }
    let mut out = String::new();
    let mut truncated = false;
    for (i, line) in raw.lines().enumerate() {
        if i == MAX_MATCHES || out.len() + line.len() > MAX_BYTES {
            truncated = true;
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    if truncated {
        out.push_str("... matches truncated; narrow the pattern or use include\n");
    }
    out
}
