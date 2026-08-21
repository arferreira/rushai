//! Shared substring-edit logic for edit and multiedit.
//!
//! Files with CRLF line endings are matched and rewritten with LF internally,
//! then converted back on write, so an LF `old_string` matches a CRLF file.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::tool::ToolError;

#[derive(Deserialize, JsonSchema, Clone)]
pub(crate) struct EditSpec {
    /// Exact text to replace.
    pub old_string: String,
    /// Replacement text.
    pub new_string: String,
    /// Replace every occurrence instead of requiring exactly one.
    #[serde(default)]
    pub replace_all: bool,
}

/// How a file terminates its lines. Mixed files are rejected rather than
/// silently normalized, since edit promises to preserve line endings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineEndings {
    Lf,
    Crlf,
}

pub(crate) fn detect_endings(text: &str) -> Result<LineEndings, ToolError> {
    let mut crlf = 0usize;
    let mut lone_lf = 0usize;
    let bytes = text.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            if i > 0 && bytes[i - 1] == b'\r' {
                crlf += 1;
            } else {
                lone_lf += 1;
            }
        }
    }
    match (crlf, lone_lf) {
        (0, _) => Ok(LineEndings::Lf),
        (_, 0) => Ok(LineEndings::Crlf),
        _ => Err(ToolError::Failed(
            "file has mixed CRLF and LF line endings; normalize it before editing".into(),
        )),
    }
}

pub(crate) fn to_lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

pub(crate) fn to_crlf(text: &str) -> String {
    to_lf(text).replace('\n', "\r\n")
}

/// Apply one edit to LF-normalized content, returning the new content.
pub(crate) fn apply_one(content: &str, edit: &EditSpec) -> Result<String, ToolError> {
    let old = to_lf(&edit.old_string);
    let new = to_lf(&edit.new_string);
    if old.is_empty() {
        return Err(ToolError::Input("old_string is empty".into()));
    }
    if old == new {
        return Err(ToolError::Input(
            "old_string and new_string are identical; nothing to change".into(),
        ));
    }
    let count = content.matches(&old).count();
    match count {
        0 => Err(ToolError::Failed(format!(
            "old_string not found:\n{}",
            edit.old_string
        ))),
        _ if count > 1 && !edit.replace_all => Err(ToolError::Failed(format!(
            "old_string is not unique: {count} matches; add context or set replace_all"
        ))),
        _ => Ok(content.replace(&old, &new)),
    }
}
