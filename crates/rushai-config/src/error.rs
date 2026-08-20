use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{}: invalid JSON: {source}", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{}: top level must be an object", path.display())]
    NotAnObject { path: PathBuf },
    #[error("invalid config at `{path}`: {message}")]
    Invalid { path: String, message: String },
    #[error("environment variable {0} is not set (referenced as ${{{0}}} in config)")]
    MissingEnvVar(String),
    #[error("unclosed ${{ in {0:?}")]
    UnclosedExpansion(String),
}
