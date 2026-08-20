//! Bridge tests run a fake `claude` shell script, so they are unix-only.
//! Windows CLI paths are covered by the assert_cmd suite in the rushai crate.
#![cfg(unix)]

use std::path::PathBuf;

use futures::StreamExt;
use rushai_protocol::{Part, Role, TokenUsage};
use rushai_provider::{
    ChatMessage, ChatRequest, ClaudeBridge, ModelInfo, Provider, ProviderError, ProviderEvent,
    StopReason, ToolDef,
};

fn fake_claude(dir: &std::path::Path, script_body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("claude");
    std::fs::write(&path, format!("#!/bin/sh\n{script_body}")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn bridge(bin: PathBuf) -> ClaudeBridge {
    ClaudeBridge::new(
        bin,
        ModelInfo {
            id: "sonnet".into(),
            context_window: 200_000,
            max_output: 8192,
            cost: None,
        },
    )
}

fn request(text: &str) -> ChatRequest {
    ChatRequest {
        messages: vec![ChatMessage {
            role: Role::User,
            parts: vec![Part::Text { text: text.into() }],
        }],
        ..Default::default()
    }
}

#[tokio::test]
async fn streams_text_usage_and_done() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake_claude(
        tmp.path(),
        r#"echo '{"type":"system","subtype":"init"}'
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"hello from the bridge"}]}}'
echo '{"type":"result","subtype":"success","usage":{"input_tokens":9,"output_tokens":3,"cache_read_input_tokens":1,"cache_creation_input_tokens":2}}'
"#,
    );
    let stream = bridge(bin).stream(&request("hi")).await.unwrap();
    let events: Vec<_> = stream
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        events,
        vec![
            ProviderEvent::TextDelta("hello from the bridge".into()),
            ProviderEvent::Usage(TokenUsage {
                input: 9,
                output: 3,
                cache_read: 1,
                cache_write: 2,
            }),
            ProviderEvent::Done {
                stop: StopReason::EndTurn
            },
        ]
    );
}

#[tokio::test]
async fn nonzero_exit_surfaces_stderr() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake_claude(
        tmp.path(),
        "echo 'invalid api key or something' >&2\nexit 3\n",
    );
    let stream = bridge(bin).stream(&request("hi")).await.unwrap();
    let events: Vec<_> = stream.collect().await;
    match events.last() {
        Some(Err(ProviderError::Stream(message))) => {
            assert!(
                message.contains("invalid api key or something"),
                "{message}"
            );
        }
        other => panic!("expected a stream error, got {other:?}"),
    }
}

#[tokio::test]
async fn missing_binary_says_how_to_fix_it() {
    let err = match bridge(PathBuf::from("/nonexistent/claude"))
        .stream(&request("hi"))
        .await
    {
        Err(err) => err,
        Ok(_) => panic!("expected an error"),
    };
    let text = err.to_string();
    assert!(text.contains("install Claude Code"), "{text}");
}

#[tokio::test]
async fn tools_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake_claude(tmp.path(), "exit 0\n");
    let mut req = request("hi");
    req.tools.push(ToolDef {
        name: "bash".into(),
        description: "run".into(),
        input_schema: serde_json::json!({}),
    });
    let err = match bridge(bin).stream(&req).await {
        Err(err) => err,
        Ok(_) => panic!("expected an error"),
    };
    assert!(matches!(err, ProviderError::Protocol(_)));
}

#[tokio::test]
async fn multi_turn_history_gets_role_markers() {
    let tmp = tempfile::tempdir().unwrap();
    // The fake echoes its received prompt back as the text so the test
    // can see exactly what the CLI would have been given.
    let bin = fake_claude(
        tmp.path(),
        r#"prompt="$2"
printf '{"type":"assistant","message":{"content":[{"type":"text","text":%s}]}}\n' "$(printf '%s' "$prompt" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')"
echo '{"type":"result","subtype":"success","usage":{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}'
"#,
    );
    let req = ChatRequest {
        messages: vec![
            ChatMessage {
                role: Role::User,
                parts: vec![Part::Text {
                    text: "first".into(),
                }],
            },
            ChatMessage {
                role: Role::Assistant,
                parts: vec![Part::Text {
                    text: "reply".into(),
                }],
            },
            ChatMessage {
                role: Role::User,
                parts: vec![Part::Text {
                    text: "second".into(),
                }],
            },
        ],
        ..Default::default()
    };
    let stream = bridge(bin).stream(&req).await.unwrap();
    let events: Vec<_> = stream
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();
    let ProviderEvent::TextDelta(echoed) = &events[0] else {
        panic!("expected text first, got {events:?}");
    };
    assert_eq!(echoed, "User:\nfirst\n\nAssistant:\nreply\n\nUser:\nsecond");
}
