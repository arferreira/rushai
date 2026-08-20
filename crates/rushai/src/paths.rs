use std::path::PathBuf;

/// Data directory: `RUSHAI_DATA_DIR`, else `$XDG_DATA_HOME/rushai`,
/// else `~/.local/share/rushai`.
pub fn data_dir() -> anyhow::Result<PathBuf> {
    if let Ok(dir) = std::env::var("RUSHAI_DATA_DIR")
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir));
    }
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME")
        && !xdg.is_empty()
    {
        return Ok(PathBuf::from(xdg).join("rushai"));
    }
    let home =
        std::env::home_dir().ok_or_else(|| anyhow::anyhow!("cannot locate home directory"))?;
    Ok(home.join(".local").join("share").join("rushai"))
}
