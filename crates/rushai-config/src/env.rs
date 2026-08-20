use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::error::ConfigError;

/// Overlay `RUSHAI_*` variables onto the config value.
/// `__` separates path segments; values parse as JSON when possible,
/// otherwise as strings.
pub(crate) fn apply_automap(root: &mut Value, env: &BTreeMap<String, String>) {
    for (key, raw) in env {
        let Some(rest) = key.strip_prefix("RUSHAI_") else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        let path: Vec<String> = rest.split("__").map(str::to_lowercase).collect();
        if path.iter().any(String::is_empty) {
            continue;
        }
        let value = serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.clone()));
        set_path(root, &path, value);
    }
}

fn set_path(root: &mut Value, path: &[String], value: Value) {
    let mut cur = root;
    for key in &path[..path.len() - 1] {
        let obj = cur.as_object_mut().expect("path nodes are objects");
        let slot = obj
            .entry(key.as_str())
            .or_insert_with(|| Value::Object(Map::new()));
        if !slot.is_object() {
            *slot = Value::Object(Map::new());
        }
        cur = slot;
    }
    let obj = cur.as_object_mut().expect("path nodes are objects");
    obj.insert(path[path.len() - 1].clone(), value);
}

/// Expand `${NAME}` in every string value. A bare `$` is left alone.
pub(crate) fn expand_strings(
    value: &mut Value,
    env: &BTreeMap<String, String>,
) -> Result<(), ConfigError> {
    match value {
        Value::String(text) => {
            if text.contains("${") {
                *text = expand(text, env)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                expand_strings(item, env)?;
            }
        }
        Value::Object(entries) => {
            for (_, item) in entries {
                expand_strings(item, env)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn expand(text: &str, env: &BTreeMap<String, String>) -> Result<String, ConfigError> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            return Err(ConfigError::UnclosedExpansion(text.to_owned()));
        };
        let name = &after[..end];
        let value = env
            .get(name)
            .ok_or_else(|| ConfigError::MissingEnvVar(name.to_owned()))?;
        out.push_str(value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}
