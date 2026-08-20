use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ConfigError;

/// Stored credentials, separate from config so config files stay shareable.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Auth {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copilot: Option<CopilotAuthEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopilotAuthEntry {
    pub github_token: String,
}

pub struct AuthStore {
    path: PathBuf,
}

impl AuthStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// A missing file is an empty store, not an error.
    pub fn load(&self) -> Result<Auth, ConfigError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Auth::default());
            }
            Err(source) => {
                return Err(ConfigError::Io {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        serde_json::from_str(&text).map_err(|source| ConfigError::Parse {
            path: self.path.clone(),
            source,
        })
    }

    pub fn save(&self, auth: &Auth) -> Result<(), ConfigError> {
        let io_err = |source| ConfigError::Io {
            path: self.path.clone(),
            source,
        };
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(io_err)?;
        }
        let text = serde_json::to_string_pretty(auth).expect("auth serializes");
        write_private(&self.path, text.as_bytes()).map_err(io_err)
    }
}

// 0600 on unix; Windows files inherit the user profile ACL, which is
// already private to the user.
#[cfg(unix)]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_missing_file_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AuthStore::new(tmp.path().join("auth.json"));
        assert_eq!(store.load().unwrap(), Auth::default());

        let auth = Auth {
            copilot: Some(CopilotAuthEntry {
                github_token: "ghu_test".into(),
            }),
        };
        store.save(&auth).unwrap();
        assert_eq!(store.load().unwrap(), auth);
    }

    #[cfg(unix)]
    #[test]
    fn auth_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("auth.json");
        let store = AuthStore::new(path.clone());
        store.save(&Auth::default()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
