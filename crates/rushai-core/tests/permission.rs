use rushai_core::permission::{PermissionError, PermissionService, PermissionSpec};
use rushai_core::store::Store;
use rushai_protocol::{Decision, PermissionRequest, SessionId};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::error::TryRecvError;

fn spec() -> PermissionSpec {
    PermissionSpec {
        tool: "bash".into(),
        action: "execute".into(),
        path: None,
        description: "run `ls`".into(),
    }
}

/// Drive one ensure() while answering its request with `decision`.
async fn ask(
    service: &PermissionService,
    rx: &mut UnboundedReceiver<PermissionRequest>,
    session: &SessionId,
    decision: Decision,
) -> (Result<(), PermissionError>, PermissionRequest) {
    let spec = spec();
    let (result, request) = tokio::join!(service.ensure(session, &spec), async {
        let request = rx.recv().await.expect("a permission request");
        service.resolve(&request.id, decision);
        request
    });
    (result, request)
}

/// The service must still be alive; Empty proves nothing was sent.
fn assert_no_request(rx: &mut UnboundedReceiver<PermissionRequest>) {
    assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
}

#[tokio::test]
async fn once_allows_one_run_then_asks_again() {
    let store = Store::open_in_memory().unwrap();
    let (service, mut rx) = PermissionService::new(false, store);
    let session = SessionId::new();

    let (result, request) = ask(&service, &mut rx, &session, Decision::Once).await;
    result.unwrap();
    assert_eq!(request.tool, "bash");
    assert_eq!(request.description, "run `ls`");

    // Once does not create a grant: the same spec asks again.
    let (result, _) = ask(&service, &mut rx, &session, Decision::Once).await;
    result.unwrap();
}

#[tokio::test]
async fn session_grant_stops_further_requests() {
    let store = Store::open_in_memory().unwrap();
    let (service, mut rx) = PermissionService::new(false, store);
    let session = SessionId::new();

    let (result, _) = ask(&service, &mut rx, &session, Decision::Session).await;
    result.unwrap();

    // Same session and key: no new request may go out.
    service.ensure(&session, &spec()).await.unwrap();
    assert_no_request(&mut rx);

    // A different session still asks.
    let other = SessionId::new();
    let (result, _) = ask(&service, &mut rx, &other, Decision::Deny).await;
    assert!(matches!(result, Err(PermissionError::Denied(_))));
}

#[tokio::test]
async fn always_grant_survives_a_service_restart() {
    let store = Store::open_in_memory().unwrap();
    let (service, mut rx) = PermissionService::new(false, store.clone());
    let session = SessionId::new();

    let (result, _) = ask(&service, &mut rx, &session, Decision::Always).await;
    result.unwrap();

    // Fresh service, same store: the grant persists, no request goes out.
    let (service, mut rx) = PermissionService::new(false, store);
    service.ensure(&SessionId::new(), &spec()).await.unwrap();
    assert_no_request(&mut rx);
}

#[tokio::test]
async fn deny_is_an_error_not_a_grant() {
    let store = Store::open_in_memory().unwrap();
    let (service, mut rx) = PermissionService::new(false, store);
    let session = SessionId::new();

    let (result, _) = ask(&service, &mut rx, &session, Decision::Deny).await;
    assert!(matches!(result, Err(PermissionError::Denied(_))));

    // Deny leaves no grant behind: the next attempt asks again.
    let (result, _) = ask(&service, &mut rx, &session, Decision::Once).await;
    result.unwrap();
}

#[tokio::test]
async fn yolo_skips_all_requests() {
    let store = Store::open_in_memory().unwrap();
    let (service, mut rx) = PermissionService::new(true, store);
    service.ensure(&SessionId::new(), &spec()).await.unwrap();
    assert_no_request(&mut rx);
}
