use std::fs;
use std::path::Path;
use std::sync::Arc;

use rushai_core::permission::PermissionService;
use rushai_core::store::Store;
use rushai_core::tool::{ToolCtx, ToolError, dispatch};
use rushai_core::tools::{GlobTool, Grep, Ls, View};
use rushai_protocol::SessionId;
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn ctx(cwd: &Path) -> ToolCtx {
    // Non-yolo service with zero grants: read tools must run anyway.
    let (service, _rx) = PermissionService::new(false, Store::open_in_memory().unwrap());
    ToolCtx {
        session: SessionId::new(),
        cwd: cwd.to_path_buf(),
        cancel: CancellationToken::new(),
        permissions: Arc::new(service),
    }
}

#[tokio::test]
async fn view_numbers_lines_and_honors_offset_and_limit() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("f.txt"), "alpha\nbeta\ngamma\ndelta\n").unwrap();
    let ctx = ctx(tmp.path());

    let out = dispatch(&View, &ctx, json!({ "path": "f.txt" }))
        .await
        .unwrap();
    assert!(out.contains("    1\talpha"), "{out}");
    assert!(out.contains("    4\tdelta"), "{out}");

    let out = dispatch(
        &View,
        &ctx,
        json!({ "path": "f.txt", "offset": 2, "limit": 2 }),
    )
    .await
    .unwrap();
    assert!(!out.contains("alpha"), "{out}");
    assert!(out.contains("    2\tbeta"), "{out}");
    assert!(out.contains("    3\tgamma"), "{out}");
    assert!(!out.contains("delta"), "{out}");
    assert!(
        out.contains("truncated"),
        "window end should note more content: {out}"
    );
}

#[tokio::test]
async fn view_rejects_binary_and_missing_files() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("bin"), b"ab\x00cd").unwrap();
    let ctx = ctx(tmp.path());

    let err = dispatch(&View, &ctx, json!({ "path": "bin" }))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ToolError::Failed(m) if m.contains("binary")),
        "{err}"
    );

    let err = dispatch(&View, &ctx, json!({ "path": "nope.txt" }))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ToolError::NotFound(p) if p == "nope.txt"),
        "{err}"
    );
}

#[tokio::test]
async fn view_caps_output() {
    let tmp = tempfile::tempdir().unwrap();
    let long = "x".repeat(200) + "\n";
    fs::write(tmp.path().join("big.txt"), long.repeat(3000)).unwrap();
    let ctx = ctx(tmp.path());

    let out = dispatch(&View, &ctx, json!({ "path": "big.txt" }))
        .await
        .unwrap();
    assert!(out.contains("truncated"), "{}", out.len());
    assert!(
        out.len() < 300 * 1024,
        "cap not applied: {} bytes",
        out.len()
    );
}

#[tokio::test]
async fn ls_sorts_and_marks_directories() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join("zdir")).unwrap();
    fs::write(tmp.path().join("afile"), "").unwrap();
    let ctx = ctx(tmp.path());

    let out = dispatch(&Ls, &ctx, json!({})).await.unwrap();
    assert_eq!(out, "afile\nzdir/\n");
}

#[tokio::test]
async fn glob_finds_nested_matches() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("src/deep")).unwrap();
    fs::write(tmp.path().join("src/a.rs"), "").unwrap();
    fs::write(tmp.path().join("src/deep/b.rs"), "").unwrap();
    fs::write(tmp.path().join("src/c.txt"), "").unwrap();
    let ctx = ctx(tmp.path());

    let out = dispatch(&GlobTool, &ctx, json!({ "pattern": "src/**/*.rs" }))
        .await
        .unwrap();
    assert!(out.contains("a.rs"), "{out}");
    assert!(out.contains("b.rs"), "{out}");
    assert!(!out.contains("c.txt"), "{out}");

    let out = dispatch(&GlobTool, &ctx, json!({ "pattern": "**/*.zig" }))
        .await
        .unwrap();
    assert_eq!(out, "no matches\n");
}

#[tokio::test]
async fn grep_reports_path_line_and_content() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("code.rs"),
        "fn main() {\n    let needle = 7;\n}\n",
    )
    .unwrap();
    fs::write(tmp.path().join("other.txt"), "no match here\n").unwrap();
    let ctx = ctx(tmp.path());

    let out = dispatch(&Grep, &ctx, json!({ "pattern": "needle" }))
        .await
        .unwrap();
    assert!(out.contains("code.rs"), "{out}");
    assert!(out.contains(":2:"), "{out}");
    assert!(out.contains("let needle = 7;"), "{out}");
    assert!(!out.contains("other.txt"), "{out}");

    let out = dispatch(
        &Grep,
        &ctx,
        json!({ "pattern": "needle", "include": "*.txt" }),
    )
    .await
    .unwrap();
    assert_eq!(out, "no matches\n");
}

#[tokio::test]
async fn read_tools_never_ask_for_permission() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("f.txt"), "data\n").unwrap();
    let (service, mut rx) = PermissionService::new(false, Store::open_in_memory().unwrap());
    let ctx = ToolCtx {
        session: SessionId::new(),
        cwd: tmp.path().to_path_buf(),
        cancel: CancellationToken::new(),
        permissions: Arc::new(service),
    };

    dispatch(&View, &ctx, json!({ "path": "f.txt" }))
        .await
        .unwrap();
    dispatch(&Ls, &ctx, json!({})).await.unwrap();
    dispatch(&GlobTool, &ctx, json!({ "pattern": "*.txt" }))
        .await
        .unwrap();
    dispatch(&Grep, &ctx, json!({ "pattern": "data" }))
        .await
        .unwrap();
    assert!(rx.try_recv().is_err(), "a read tool asked for permission");
}
