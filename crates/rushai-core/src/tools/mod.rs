mod glob_tool;
mod grep;
mod ls;
mod view;

use std::path::{Path, PathBuf};

pub use glob_tool::GlobTool;
pub use grep::Grep;
pub use ls::Ls;
pub use view::View;

use crate::tool::ToolError;

/// Resolve a tool path argument against the working directory.
fn resolve(cwd: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

fn missing(path: &str) -> ToolError {
    ToolError::Failed(format!("{path} does not exist"))
}
