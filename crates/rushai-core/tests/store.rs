use rushai_core::store::{Store, StoredMessage};
use rushai_protocol::{FinishReason, MessageId, Part, Role, SessionId, TokenUsage};
use serde_json::json;

fn message(session: &SessionId, parts: Vec<Part>) -> StoredMessage {
    StoredMessage {
        id: MessageId::new(),
        session: session.clone(),
        role: Role::Assistant,
        provider: Some("anthropic".into()),
        model: Some("claude-opus-5".into()),
        parts,
        is_summary: false,
        created_at: 1,
    }
}

#[tokio::test]
async fn session_round_trip() {
    let store = Store::open_in_memory().unwrap();
    let parent = store.create_session("parent".into(), None).await.unwrap();
    let child = store
        .create_session("child".into(), Some(parent.id.clone()))
        .await
        .unwrap();

    assert_eq!(store.session(&child.id).await.unwrap().unwrap(), child);
    assert_eq!(child.parent.as_ref(), Some(&parent.id));
    assert!(
        store
            .session(&SessionId::from("missing"))
            .await
            .unwrap()
            .is_none()
    );

    let all = store.sessions().await.unwrap();
    assert_eq!(all.len(), 2);
}

#[tokio::test]
async fn message_parts_round_trip() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session("s".into(), None).await.unwrap();
    let msg = message(
        &session.id,
        vec![
            Part::Text {
                text: "hello".into(),
            },
            Part::Reasoning {
                text: "thinking".into(),
                signature: Some("sig".into()),
            },
            Part::ToolCall {
                id: "c1".into(),
                name: "bash".into(),
                input: json!({ "command": "ls" }),
            },
            Part::ToolResult {
                id: "c1".into(),
                content: "Cargo.toml".into(),
                is_error: false,
            },
            Part::Finish {
                reason: FinishReason::EndTurn,
                usage: TokenUsage {
                    input: 10,
                    output: 4,
                    cache_read: 2,
                    cache_write: 1,
                },
            },
        ],
    );
    store.save_message(&msg).await.unwrap();

    let stored = store.messages(&session.id).await.unwrap();
    assert_eq!(stored, vec![msg]);
}

#[tokio::test]
async fn save_message_upserts() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session("s".into(), None).await.unwrap();
    let mut msg = message(
        &session.id,
        vec![Part::Text {
            text: "draft".into(),
        }],
    );
    store.save_message(&msg).await.unwrap();

    msg.parts = vec![Part::Text {
        text: "final".into(),
    }];
    store.save_message(&msg).await.unwrap();

    let stored = store.messages(&session.id).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].parts, msg.parts);
}

#[tokio::test]
async fn messages_keep_creation_order() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session("s".into(), None).await.unwrap();
    for (at, text) in [(3, "third"), (1, "first"), (2, "second")] {
        let mut msg = message(&session.id, vec![Part::Text { text: text.into() }]);
        msg.created_at = at;
        store.save_message(&msg).await.unwrap();
    }
    let texts: Vec<_> = store
        .messages(&session.id)
        .await
        .unwrap()
        .into_iter()
        .map(|m| match &m.parts[0] {
            Part::Text { text } => text.clone(),
            other => panic!("unexpected part {other:?}"),
        })
        .collect();
    assert_eq!(texts, ["first", "second", "third"]);
}

#[tokio::test]
async fn deleting_a_session_removes_its_messages() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session("s".into(), None).await.unwrap();
    let msg = message(&session.id, vec![Part::Text { text: "x".into() }]);
    store.save_message(&msg).await.unwrap();

    store.delete_session(&session.id).await.unwrap();
    assert!(store.session(&session.id).await.unwrap().is_none());
    assert!(store.messages(&session.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn data_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rushai.db");

    let store = Store::open(&path).unwrap();
    let session = store
        .create_session("persisted".into(), None)
        .await
        .unwrap();
    let msg = message(
        &session.id,
        vec![Part::Text {
            text: "kept".into(),
        }],
    );
    store.save_message(&msg).await.unwrap();
    drop(store);

    let store = Store::open(&path).unwrap();
    assert_eq!(
        store.session(&session.id).await.unwrap().unwrap().title,
        "persisted"
    );
    assert_eq!(store.messages(&session.id).await.unwrap(), vec![msg]);
}
