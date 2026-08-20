use std::path::{Path, PathBuf};

/// Directories from the git root down to `cwd`, inclusive.
/// Just `[cwd]` outside a git repository.
pub(crate) fn dir_chain(cwd: &Path) -> Vec<PathBuf> {
    let mut chain = vec![cwd.to_path_buf()];
    if git_root(cwd).is_some() {
        let mut dir = cwd;
        while !dir.join(".git").exists() {
            match dir.parent() {
                Some(parent) => {
                    chain.push(parent.to_path_buf());
                    dir = parent;
                }
                None => break,
            }
        }
    }
    chain.reverse();
    chain
}

/// Existing config files, lowest to highest precedence.
pub(crate) fn config_files(cwd: &Path, user_config_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Some(dir) = user_config_dir {
        files.push(dir.join("rushai.json"));
    }
    for dir in dir_chain(cwd) {
        files.push(dir.join("rushai.json"));
        files.push(dir.join(".rushai.json"));
    }
    files.retain(|path| path.is_file());
    files
}

pub(crate) fn user_config_dir() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return Some(PathBuf::from(xdg).join("rushai"));
    }
    std::env::home_dir().map(|home| home.join(".config").join("rushai"))
}

fn git_root(cwd: &Path) -> Option<PathBuf> {
    let mut dir = cwd;
    loop {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}
