//! Bash must not hand provider credentials to spawned commands.
//!
//! This is the only test in its binary on purpose: it mutates process
//! environment, which is unsound to do while other threads read it.

#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;

use rushai_core::permission::PermissionService;
use rushai_core::store::Store;
use rushai_core::tool::{ToolCtx, dispatch};
use rushai_core::tools::Bash;
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn ctx(cwd: &Path) -> ToolCtx {
    let store = Store::open_in_memory().unwrap();
    let (service, _rx) = PermissionService::new(true, store);
    ToolCtx {
        session: rushai_protocol::SessionId::new(),
        cwd: cwd.to_path_buf(),
        cancel: CancellationToken::new(),
        permissions: Arc::new(service),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn bash_scrubs_provider_credentials() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path());
    // SAFETY: single-threaded runtime, only test in this binary, so nothing
    // else reads the environment concurrently.
    unsafe { std::env::set_var("ANTHROPIC_API_KEY", "sk-should-not-leak") };
    let out = dispatch(
        &Bash::with_shell("/bin/sh"),
        &ctx,
        json!({ "command": "echo key=[${ANTHROPIC_API_KEY:-}]" }),
    )
    .await
    .unwrap();
    unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };
    assert!(out.contains("key=[]"), "credential leaked to child: {out}");
    assert!(!out.contains("sk-should-not-leak"), "{out}");
}
