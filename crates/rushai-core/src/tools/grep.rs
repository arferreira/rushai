use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use rushai_provider::ToolDef;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{missing, resolve};
use crate::permission::PermissionSpec;
use crate::tool::{RunToken, Tool, ToolCtx, ToolError, parse_input, schema_for};

const MAX_MATCHES: usize = 200;
const MAX_BYTES: usize = 100 * 1024;
const READ_CAP: usize = MAX_BYTES * 2;
const MAX_FILES: usize = 10_000;

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

    async fn run(&self, ctx: &ToolCtx, input: Value, _run: RunToken) -> Result<String, ToolError> {
        let input: Input = parse_input(input)?;
        let shown = input.path.as_deref().unwrap_or(".");
        let target = resolve(&ctx.cwd, shown);
        if !target.exists() {
            return Err(missing(shown));
        }
        let raw = match rg(&input, &target, ctx).await {
            Some(result) => result?,
            None => {
                let cancel = ctx.cancel.clone();
                let pattern = input.pattern.clone();
                let include = input.include.clone();
                tokio::task::spawn_blocking(move || walk(&pattern, include, &target, &cancel))
                    .await
                    .map_err(|e| ToolError::Failed(format!("search task failed: {e}")))??
            }
        };
        Ok(cap(raw))
    }
}

/// Run rg and return `path:line:content` lines. None when rg is not
/// installed (any other spawn failure is a real error).
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
    // stderr is discarded, not piped: an unread pipe would deadlock rg.
    cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    cmd.kill_on_drop(true);

    match cmd.spawn() {
        Ok(child) => Some(rg_read(child, ctx).await),
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => Some(Err(e.into())),
    }
}

async fn rg_read(mut child: tokio::process::Child, ctx: &ToolCtx) -> Result<String, ToolError> {
    use tokio::io::AsyncReadExt;

    let stdout = child.stdout.take().expect("piped stdout");
    let mut reader = tokio::io::BufReader::new(stdout).take(READ_CAP as u64 + 1);
    let mut buf = Vec::new();
    tokio::select! {
        result = reader.read_to_end(&mut buf) => {
            result?;
        }
        _ = ctx.cancel.cancelled() => {
            let _ = child.start_kill();
            return Err(ToolError::Canceled);
        }
    }
    if buf.len() > READ_CAP {
        // Cap hit: stop rg instead of draining it; we already have more
        // output than we will show, and a blocked rg would deadlock wait().
        let _ = child.start_kill();
        let _ = child.wait().await;
    } else {
        let status = child.wait().await?;
        // rg exits 0 on matches, 1 on none; anything else, including a
        // signal death (code() == None), is a failure.
        if !matches!(status.code(), Some(0) | Some(1)) {
            return Err(ToolError::Failed("rg failed".into()));
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Fallback without rg: regex walk producing the same `path:line:content`
/// shape. Symlinks are skipped entirely, so cycles cannot occur, and the
/// number of files scanned is bounded.
fn walk(
    pattern: &str,
    include: Option<String>,
    target: &Path,
    cancel: &CancellationToken,
) -> Result<String, ToolError> {
    let re =
        regex::Regex::new(pattern).map_err(|e| ToolError::Input(format!("bad pattern: {e}")))?;
    let include = include
        .as_deref()
        .map(glob::Pattern::new)
        .transpose()
        .map_err(|e| ToolError::Input(format!("bad include: {e}")))?;
    let mut out = String::new();
    let mut scanned = 0usize;
    let mut stack: Vec<PathBuf> = vec![target.to_path_buf()];
    let mut first = true;
    while let Some(path) = stack.pop() {
        if cancel.is_cancelled() {
            return Err(ToolError::Canceled);
        }
        // The explicit target may be a symlink (follow it); below it,
        // symlinks are never followed.
        let meta = if first {
            std::fs::metadata(&path)
        } else {
            std::fs::symlink_metadata(&path)
        };
        first = false;
        let Ok(meta) = meta else { continue };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name == ".git" || name == "target" || name == "node_modules" {
                continue;
            }
            for entry in std::fs::read_dir(&path)?.flatten() {
                stack.push(entry.path());
            }
            continue;
        }
        scanned += 1;
        if scanned > MAX_FILES {
            out.push_str("... search stopped: too many files, narrow the path\n");
            break;
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
            if out.len() > READ_CAP {
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
