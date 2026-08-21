use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rushai_core::permission::{PermissionError, PermissionService, PermissionSpec};
use rushai_core::store::Store;
use rushai_core::tool::{RunToken, Tool, ToolCtx, ToolError, dispatch};
use rushai_protocol::{Decision, SessionId};
use rushai_provider::ToolDef;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// A tool that requires a grant and records whether it ever ran.
struct Gated {
    ran: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl Tool for Gated {
    fn name(&self) -> &'static str {
        "danger"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "danger".into(),
            description: "test tool".into(),
            input_schema: json!({ "type": "object" }),
        }
    }

    fn permission(&self, _ctx: &ToolCtx, _input: &Value) -> Option<PermissionSpec> {
        Some(PermissionSpec::new(
            "danger",
            "write",
            Some("victim.txt".into()),
            "write victim.txt",
        ))
    }

    async fn run(
        &self,
        _ctx: &ToolCtx,
        _input: Value,
        _run: RunToken,
    ) -> Result<String, ToolError> {
        self.ran.store(true, Ordering::SeqCst);
        Ok("ran".into())
    }
}

fn harness() -> (
    ToolCtx,
    tokio::sync::mpsc::UnboundedReceiver<rushai_protocol::PermissionRequest>,
) {
    let (service, rx) = PermissionService::new(false, Store::open_in_memory().unwrap());
    let ctx = ToolCtx {
        session: SessionId::new(),
        cwd: std::env::temp_dir(),
        cancel: CancellationToken::new(),
        permissions: Arc::new(service),
    };
    (ctx, rx)
}

#[tokio::test]
async fn deny_blocks_execution_entirely() {
    let (ctx, mut rx) = harness();
    let ran = Arc::new(AtomicBool::new(false));
    let tool = Gated { ran: ran.clone() };

    let (result, request) = tokio::join!(dispatch(&tool, &ctx, json!({})), async {
        let request = rx.recv().await.expect("a permission request");
        ctx.permissions.resolve(&request.id, Decision::Deny);
        request
    });

    assert!(matches!(
        result,
        Err(ToolError::Permission(PermissionError::Denied(_)))
    ));
    assert!(!ran.load(Ordering::SeqCst), "denied tool still ran");
    // dispatch resolved the relative spec path to an absolute one.
    let path = request.path.expect("spec path");
    assert!(std::path::Path::new(&path).is_absolute(), "{path}");
    assert!(path.ends_with("victim.txt"), "{path}");
}

#[tokio::test]
async fn grant_allows_execution() {
    let (ctx, mut rx) = harness();
    let ran = Arc::new(AtomicBool::new(false));
    let tool = Gated { ran: ran.clone() };

    let (result, _) = tokio::join!(dispatch(&tool, &ctx, json!({})), async {
        let request = rx.recv().await.expect("a permission request");
        ctx.permissions.resolve(&request.id, Decision::Once);
    });

    assert_eq!(result.unwrap(), "ran");
    assert!(ran.load(Ordering::SeqCst));
}
