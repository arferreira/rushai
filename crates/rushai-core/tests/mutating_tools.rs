use std::path::Path;
use std::sync::Arc;

use std::collections::HashSet;

use rushai_core::permission::PermissionService;
use rushai_core::store::Store;
#[cfg(unix)]
use rushai_core::tool::ToolError;
use rushai_core::tool::{Tool, ToolCtx, dispatch};
#[cfg(unix)]
use rushai_core::tools::Bash;
use rushai_core::tools::{Edit, MultiEdit, Todos, Write, registry};
use serde_json::json;
use tokio_util::sync::CancellationToken;

/// A yolo context: permission always granted, so tests exercise the tool body.
async fn ctx(cwd: &Path) -> (ToolCtx, Store) {
    let store = Store::open_in_memory().unwrap();
    let (service, _rx) = PermissionService::new(true, store.clone());
    let session = store.create_session("t".into(), None).await.unwrap();
    (
        ToolCtx {
            session: session.id,
            cwd: cwd.to_path_buf(),
            cancel: CancellationToken::new(),
            permissions: Arc::new(service),
        },
        store,
    )
}

#[tokio::test]
async fn write_creates_file_and_parents() {
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, _s) = ctx(tmp.path()).await;
    dispatch(
        &Write,
        &ctx,
        json!({ "path": "sub/dir/hello.txt", "content": "hi" }),
    )
    .await
    .unwrap();
    let written = std::fs::read_to_string(tmp.path().join("sub/dir/hello.txt")).unwrap();
    assert_eq!(written, "hi");
}

#[tokio::test]
async fn edit_replaces_unique_match() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f.txt"), "let x = 1;\nlet y = 2;\n").unwrap();
    let (ctx, _s) = ctx(tmp.path()).await;
    dispatch(
        &Edit,
        &ctx,
        json!({ "path": "f.txt", "old_string": "let x = 1;", "new_string": "let x = 42;" }),
    )
    .await
    .unwrap();
    let after = std::fs::read_to_string(tmp.path().join("f.txt")).unwrap();
    assert_eq!(after, "let x = 42;\nlet y = 2;\n");
}

#[tokio::test]
async fn edit_ambiguous_match_reports_count() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f.txt"), "a\na\na\n").unwrap();
    let (ctx, _s) = ctx(tmp.path()).await;
    let err = dispatch(
        &Edit,
        &ctx,
        json!({ "path": "f.txt", "old_string": "a", "new_string": "b" }),
    )
    .await
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains('3'), "count missing: {msg}");
    // File untouched on error.
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("f.txt")).unwrap(),
        "a\na\na\n"
    );
}

#[tokio::test]
async fn edit_preserves_crlf_with_lf_old_string() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f.txt"), "one\r\ntwo\r\nthree\r\n").unwrap();
    let (ctx, _s) = ctx(tmp.path()).await;
    dispatch(
        &Edit,
        &ctx,
        json!({ "path": "f.txt", "old_string": "two", "new_string": "TWO" }),
    )
    .await
    .unwrap();
    let raw = std::fs::read(tmp.path().join("f.txt")).unwrap();
    assert_eq!(raw, b"one\r\nTWO\r\nthree\r\n");
}

#[tokio::test]
async fn edit_handles_multibyte_boundaries() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f.txt"), "café → bar 🦀\n").unwrap();
    let (ctx, _s) = ctx(tmp.path()).await;
    dispatch(
        &Edit,
        &ctx,
        json!({ "path": "f.txt", "old_string": "→ bar 🦀", "new_string": "→ baz 🦀🦀" }),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("f.txt")).unwrap(),
        "café → baz 🦀🦀\n"
    );
}

#[tokio::test]
async fn multiedit_is_atomic_on_later_failure() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f.txt"), "alpha beta gamma\n").unwrap();
    let (ctx, _s) = ctx(tmp.path()).await;
    let err = dispatch(
        &MultiEdit,
        &ctx,
        json!({
            "path": "f.txt",
            "edits": [
                { "old_string": "alpha", "new_string": "ALPHA" },
                { "old_string": "nonexistent", "new_string": "x" }
            ]
        }),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("edit 2"), "{err}");
    // First edit must not have landed.
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("f.txt")).unwrap(),
        "alpha beta gamma\n"
    );
}

#[tokio::test]
async fn multiedit_applies_all_in_order() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f.txt"), "1 2 3\n").unwrap();
    let (ctx, _s) = ctx(tmp.path()).await;
    dispatch(
        &MultiEdit,
        &ctx,
        json!({
            "path": "f.txt",
            "edits": [
                { "old_string": "1", "new_string": "one" },
                { "old_string": "2", "new_string": "two" }
            ]
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("f.txt")).unwrap(),
        "one two 3\n"
    );
}

#[tokio::test]
async fn registry_has_unique_tool_names() {
    let store = Store::open_in_memory().unwrap();
    let tools = registry(store);
    let mut seen = HashSet::new();
    for tool in &tools {
        assert!(
            seen.insert(tool.name()),
            "duplicate tool name {}",
            tool.name()
        );
    }
    assert!(tools.len() >= 9, "expected the full builtin set");
}

#[tokio::test]
async fn todos_unknown_session_errors() {
    let store = Store::open_in_memory().unwrap();
    let err = store
        .set_todos(&rushai_protocol::SessionId::new(), &[])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("unknown session"), "{err}");
}

#[tokio::test]
async fn todos_round_trip_through_store() {
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, store) = ctx(tmp.path()).await;
    let out = dispatch(
        &Todos::new(store.clone()),
        &ctx,
        json!({ "todos": [
            { "text": "write tests", "done": true },
            { "text": "ship it", "done": false }
        ]}),
    )
    .await
    .unwrap();
    assert!(out.contains("[x] write tests"), "{out}");
    assert!(out.contains("[ ] ship it"), "{out}");
    let stored = store.todos(&ctx.session).await.unwrap();
    assert_eq!(stored.len(), 2);
    assert!(stored[0].done);
}

#[cfg(unix)]
#[tokio::test]
async fn bash_runs_and_reports_exit() {
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, _s) = ctx(tmp.path()).await;
    let out = dispatch(
        &Bash::with_shell("/bin/sh"),
        &ctx,
        json!({ "command": "echo hello && exit 3" }),
    )
    .await
    .unwrap();
    assert!(out.contains("hello"), "{out}");
    assert!(out.contains("[exit 3]"), "{out}");
}

#[tokio::test]
async fn edit_replace_all_changes_every_match() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f.txt"), "x x x\n").unwrap();
    let (ctx, _s) = ctx(tmp.path()).await;
    dispatch(
        &Edit,
        &ctx,
        json!({ "path": "f.txt", "old_string": "x", "new_string": "y", "replace_all": true }),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("f.txt")).unwrap(),
        "y y y\n"
    );
}

#[tokio::test]
async fn edit_refuses_mixed_line_endings() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f.txt"), "one\r\ntwo\nthree\r\n").unwrap();
    let (ctx, _s) = ctx(tmp.path()).await;
    let err = dispatch(
        &Edit,
        &ctx,
        json!({ "path": "f.txt", "old_string": "two", "new_string": "TWO" }),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("mixed"), "{err}");
    // File untouched.
    assert_eq!(
        std::fs::read(tmp.path().join("f.txt")).unwrap(),
        b"one\r\ntwo\nthree\r\n"
    );
}

#[tokio::test]
async fn edit_noop_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f.txt"), "same\n").unwrap();
    let (ctx, _s) = ctx(tmp.path()).await;
    let err = dispatch(
        &Edit,
        &ctx,
        json!({ "path": "f.txt", "old_string": "same", "new_string": "same" }),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("identical"), "{err}");
}

#[tokio::test]
async fn write_refuses_relative_escape_but_allows_absolute() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let (ctx, _s) = ctx(&work).await;

    // A relative path climbing out of cwd is refused.
    let err = dispatch(
        &Write,
        &ctx,
        json!({ "path": "../escape.txt", "content": "x" }),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("outside the workspace"), "{err}");
    assert!(!tmp.path().join("escape.txt").exists());

    // An explicit absolute path is allowed (the human saw the real target).
    let outside = tmp.path().join("deliberate.txt");
    dispatch(
        &Write,
        &ctx,
        json!({ "path": outside.to_str().unwrap(), "content": "ok" }),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "ok");
}

#[tokio::test]
async fn write_permission_describes_real_target_via_ctx_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("proj");
    std::fs::create_dir(&work).unwrap();
    let (ctx, _s) = ctx(&work).await;
    // permission() must resolve against ctx.cwd, not the process cwd.
    let spec = Write
        .permission(&ctx, &json!({ "path": "new.txt", "content": "" }))
        .unwrap();
    assert!(spec.description.contains("create"), "{}", spec.description);
    assert!(
        spec.description
            .contains(&work.join("new.txt").display().to_string())
            || spec.description.contains("new.txt"),
        "{}",
        spec.description
    );
}

#[cfg(unix)]
#[tokio::test]
async fn bash_grant_is_scoped_to_one_command() {
    // A session grant for one command must not authorize a different one.
    let tmp = tempfile::tempdir().unwrap();
    let ls = Bash::with_shell("/bin/sh")
        .permission(&dummy_ctx(tmp.path()), &json!({ "command": "ls -la" }))
        .unwrap();
    let rm = Bash::with_shell("/bin/sh")
        .permission(&dummy_ctx(tmp.path()), &json!({ "command": "rm file" }))
        .unwrap();
    assert_ne!(ls.action, rm.action, "distinct commands share a grant key");
    assert!(!ls.persistable, "bash grants must not persist");
    // Whitespace-only differences collapse to the same key.
    let ls2 = Bash::with_shell("/bin/sh")
        .permission(&dummy_ctx(tmp.path()), &json!({ "command": "ls   -la" }))
        .unwrap();
    assert_eq!(ls.action, ls2.action);
}

#[cfg(unix)]
fn dummy_ctx(cwd: &Path) -> ToolCtx {
    let store = Store::open_in_memory().unwrap();
    let (service, _rx) = PermissionService::new(true, store);
    ToolCtx {
        session: rushai_protocol::SessionId::new(),
        cwd: cwd.to_path_buf(),
        cancel: CancellationToken::new(),
        permissions: Arc::new(service),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn bash_timeout_upper_bound_is_clamped() {
    // A 999-minute request must not wait 999 minutes: the clamp caps it, and
    // a quick command returns immediately regardless of the huge input.
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, _s) = ctx(tmp.path()).await;
    let out = dispatch(
        &Bash::with_shell("/bin/sh"),
        &ctx,
        json!({ "command": "echo hi", "timeout": 60000 }),
    )
    .await
    .unwrap();
    assert!(out.contains("hi"), "{out}");
}

#[tokio::test]
async fn write_refuses_when_permission_denied() {
    let tmp = tempfile::tempdir().unwrap();
    // Non-yolo service: the write must be denied and leave no file.
    let store = Store::open_in_memory().unwrap();
    let (service, mut rx) = PermissionService::new(false, store.clone());
    let session = store.create_session("t".into(), None).await.unwrap();
    let ctx = ToolCtx {
        session: session.id,
        cwd: tmp.path().to_path_buf(),
        cancel: CancellationToken::new(),
        permissions: Arc::new(service),
    };

    let (result, _) = tokio::join!(
        dispatch(
            &Write,
            &ctx,
            json!({ "path": "secret.txt", "content": "x" })
        ),
        async {
            let request = rx.recv().await.expect("a permission request");
            ctx.permissions
                .resolve(&request.id, rushai_protocol::Decision::Deny);
        }
    );
    assert!(result.is_err(), "denied write returned Ok");
    assert!(
        !tmp.path().join("secret.txt").exists(),
        "denied write hit disk"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn bash_refuses_banned_command() {
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, _s) = ctx(tmp.path()).await;
    let err = dispatch(
        &Bash::with_shell("/bin/sh"),
        &ctx,
        json!({ "command": "rm -rf /" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::Failed(m) if m.contains("refused")));
}

#[cfg(unix)]
#[tokio::test]
async fn bash_output_is_capped() {
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, _s) = ctx(tmp.path()).await;
    let out = dispatch(
        &Bash::with_shell("/bin/sh"),
        &ctx,
        json!({ "command": "yes rushai | head -c 300000" }),
    )
    .await
    .unwrap();
    assert!(out.contains("truncated"), "not truncated");
    assert!(out.len() < 200 * 1024, "cap not enforced: {}", out.len());
}

#[cfg(unix)]
#[tokio::test]
async fn bash_timeout_kills_child_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, _s) = ctx(tmp.path()).await;
    // A grandchild sleep writes its pid, then the shell waits on it. On
    // timeout the whole group must die, so the grandchild is gone after.
    let marker = tmp.path().join("grandchild.pid");
    let cmd = format!("sh -c 'echo $$ > {} ; sleep 30' & wait", marker.display());
    let start = std::time::Instant::now();
    let err = dispatch(
        &Bash::with_shell("/bin/sh"),
        &ctx,
        json!({ "command": cmd, "timeout": 1 }),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("timed out"), "{err}");
    assert!(start.elapsed() < std::time::Duration::from_secs(10));

    let pid: i32 = std::fs::read_to_string(&marker)
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    // Poll until the pid is gone. kill(pid, 0) returns ESRCH once the process
    // is fully reaped; EPERM would mean alive-but-not-ours, and errno 0 (a
    // zombie still in the table) counts as not-yet-dead, so we keep waiting.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let dead = loop {
        let rc = unsafe { libc::kill(pid, 0) };
        if rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            break true;
        }
        if std::time::Instant::now() >= deadline {
            break false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    assert!(dead, "grandchild {pid} survived the timeout");
}

#[cfg(unix)]
#[tokio::test]
async fn bash_cancellation_stops_the_command() {
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, _s) = ctx(tmp.path()).await;
    let cancel = ctx.cancel.clone();
    let handle = tokio::spawn(async move {
        dispatch(
            &Bash::with_shell("/bin/sh"),
            &ctx,
            json!({ "command": "sleep 30" }),
        )
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    cancel.cancel();
    let result = handle.await.unwrap();
    assert!(matches!(result, Err(ToolError::Canceled)));
}
