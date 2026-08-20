//! Configuration for rushai.
//!
//! Sources, lowest to highest precedence:
//! 1. `$XDG_CONFIG_HOME/rushai/rushai.json` (or `~/.config/rushai/rushai.json`)
//! 2. `rushai.json` then `.rushai.json` in each directory from the git root
//!    down to the working directory (nearer directories win)
//! 3. `RUSHAI_*` environment variables, `__` separating path segments
//!    (`RUSHAI_PROVIDERS__ANTHROPIC__API_KEY` sets `providers.anthropic.api_key`)
//!
//! String values may reference environment variables as `${NAME}`.
//! `.env` files at the git root and working directory are loaded into the
//! environment map; real environment variables win over them.

mod auth;
mod discover;
mod env;
mod error;
mod merge;

use std::collections::BTreeMap;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use auth::{Auth, AuthStore, CopilotAuthEntry};
pub use error::ConfigError;

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Default model as `provider/model-id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// Skip all permission prompts.
    #[serde(default)]
    pub yolo: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub providers: BTreeMap<String, ProviderConfig>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

impl Config {
    pub fn load(cwd: impl Into<PathBuf>) -> Result<Config, ConfigError> {
        Loader::new(cwd).load()
    }
}

pub fn json_schema() -> schemars::Schema {
    schemars::schema_for!(Config)
}

/// All inputs to config resolution, injectable for tests.
pub struct Loader {
    pub cwd: PathBuf,
    pub user_config_dir: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
}

impl Loader {
    /// Build from the process environment, loading `.env` files along the way.
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        let cwd = cwd.into();
        let mut env = BTreeMap::new();
        for dir in discover::dir_chain(&cwd) {
            let dotenv = dir.join(".env");
            if let Ok(iter) = dotenvy::from_path_iter(&dotenv) {
                for (key, value) in iter.flatten() {
                    env.insert(key, value);
                }
            }
        }
        env.extend(std::env::vars());
        Self {
            cwd,
            user_config_dir: discover::user_config_dir(),
            env,
        }
    }

    pub fn load(&self) -> Result<Config, ConfigError> {
        let mut merged = Value::Object(serde_json::Map::new());
        for path in discover::config_files(&self.cwd, self.user_config_dir.as_deref()) {
            let text = std::fs::read_to_string(&path).map_err(|source| ConfigError::Io {
                path: path.clone(),
                source,
            })?;
            let value: Value =
                serde_json::from_str(&text).map_err(|source| ConfigError::Parse {
                    path: path.clone(),
                    source,
                })?;
            if !value.is_object() {
                return Err(ConfigError::NotAnObject { path });
            }
            merge::deep_merge(&mut merged, value);
        }
        env::expand_strings(&mut merged, &self.env)?;
        env::apply_automap(&mut merged, &self.env);

        serde_path_to_error::deserialize(merged).map_err(|err| ConfigError::Invalid {
            path: err.path().to_string(),
            message: err.into_inner().to_string(),
        })
    }
}
