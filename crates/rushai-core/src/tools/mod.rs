mod bash;
mod edit;
mod edits;
mod glob_tool;
mod grep;
mod ls;
mod multiedit;
mod todos;
mod view;
mod write;

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use bash::Bash;
pub use edit::Edit;
pub use glob_tool::GlobTool;
pub use grep::Grep;
pub use ls::Ls;
pub use multiedit::MultiEdit;
pub use todos::Todos;
pub use view::View;
pub use write::Write;

use crate::store::Store;
use crate::tool::{Tool, ToolError};

/// The full built-in tool set. One construction point for the agent loop.
pub fn registry(store: Store) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(View),
        Arc::new(Ls),
        Arc::new(GlobTool),
        Arc::new(Grep),
        Arc::new(Write),
        Arc::new(Edit),
        Arc::new(MultiEdit),
        Arc::new(Bash::new()),
        Arc::new(Todos::new(store)),
    ]
}

/// Resolve a tool path argument against the working directory. Absolute paths
/// are allowed; read tools may read anywhere, and write tools enforce
/// containment separately via [`resolve_write`].
fn resolve(cwd: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

/// Resolve the real filesystem location a path points at, following symlinks
/// on the nearest existing ancestor so `..` and symlink escapes are visible.
/// The non-existing tail (a file about to be created) is appended verbatim.
fn real_target(cwd: &Path, path: &str) -> PathBuf {
    let absolute = resolve(cwd, path);
    let mut existing = absolute.as_path();
    let mut tail = PathBuf::new();
    loop {
        if existing.exists() {
            let mut real = existing
                .canonicalize()
                .unwrap_or_else(|_| existing.to_path_buf());
            real.push(&tail);
            return real;
        }
        match existing.parent() {
            Some(parent) => {
                let name = existing.strip_prefix(parent).unwrap_or(Path::new(""));
                tail = if tail.as_os_str().is_empty() {
                    name.to_path_buf()
                } else {
                    name.join(&tail)
                };
                existing = parent;
            }
            None => return absolute,
        }
    }
}

/// Enforce write containment. Relative paths must resolve to a location inside
/// the working directory (after symlinks); absolute paths are allowed but the
/// caller must surface the real target in the permission prompt. Returns the
/// path to actually write.
fn resolve_write(cwd: &Path, path: &str) -> Result<PathBuf, ToolError> {
    let absolute = resolve(cwd, path);
    if Path::new(path).is_absolute() {
        return Ok(absolute);
    }
    let real = real_target(cwd, path);
    let cwd_real = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    if !real.starts_with(&cwd_real) {
        return Err(ToolError::Failed(format!(
            "{path} resolves to {} outside the workspace; pass an absolute path to write there deliberately",
            real.display()
        )));
    }
    Ok(absolute)
}

fn missing(path: &str) -> ToolError {
    ToolError::NotFound(path.to_owned())
}

/// Write bytes durably: create a sibling temp file, then rename over the
/// target. A crash or ENOSPC leaves the original intact instead of truncated.
async fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ToolError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let name = path
        .file_name()
        .map_or_else(|| ".rushai".into(), |n| n.to_owned());
    let mut tmp = path.to_path_buf();
    tmp.set_file_name({
        let mut n = std::ffi::OsString::from(".");
        n.push(&name);
        n.push(".rushai.tmp");
        n
    });
    tokio::fs::write(&tmp, bytes).await?;
    match tokio::fs::rename(&tmp, path).await {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            Err(e.into())
        }
    }
}
