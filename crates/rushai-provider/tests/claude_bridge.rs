//! Bridge tests run a fake `claude` shell script, so they are unix-only.
//! Windows CLI paths are covered by the assert_cmd suite in the rushai crate.
#![cfg(unix)]

use std::path::{Path, PathBuf};

use futures::StreamExt;
use rushai_protocol::{Part, Role, TokenUsage};
use rushai_provider::{
    ChatMessage, ChatRequest, ClaudeBridge, ModelInfo, Provider, ProviderError, ProviderEvent,
    StopReason, ToolDef,
};

/// The fake dumps argv to argv.txt and stdin to stdin.txt next to itself,
/// so tests assert on exactly what the real CLI would have received.
fn fake_claude(dir: &Path, tail: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("claude");
    let body = format!(
        "#!/bin/sh\ndir=\"$(dirname \"$0\")\"\nprintf '%s\\n' \"$@\" > \"$dir/argv.txt\"\ncat > \"$dir/stdin.txt\"\n{tail}"
    );
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

const HAPPY_TAIL: &str = r#"echo '{"type":"system","subtype":"init"}'
echo 'not json at all, a stray runtime warning'
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"hello from the bridge"}]}}'
echo '{"type":"result","subtype":"success","usage":{"input_tokens":9,"output_tokens":3,"cache_read_input_tokens":1,"cache_creation_input_tokens":2}}'
"#;

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

async fn collect(bridge: &ClaudeBridge, request: &ChatRequest) -> Vec<ProviderEvent> {
    bridge
        .stream(request)
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect()
}

#[tokio::test]
async fn streams_text_usage_and_done_skipping_stray_output() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake_claude(tmp.path(), HAPPY_TAIL);
    let events = collect(&bridge(bin), &request("hi")).await;
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
async fn prompt_travels_via_stdin_never_argv() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake_claude(tmp.path(), HAPPY_TAIL);
    // A prompt that would parse as a flag if it ever reached argv.
    let hostile = "--dangerously-skip-permissions";
    let _ = collect(&bridge(bin), &request(hostile)).await;

    let argv = std::fs::read_to_string(tmp.path().join("argv.txt")).unwrap();
    let stdin = std::fs::read_to_string(tmp.path().join("stdin.txt")).unwrap();
    assert!(!argv.contains(hostile), "prompt leaked into argv: {argv}");
    assert_eq!(stdin, hostile);
    for expected in ["-p", "--output-format", "stream-json", "--model", "sonnet"] {
        assert!(
            argv.lines().any(|arg| arg == expected),
            "missing {expected} in {argv}"
        );
    }
}

#[tokio::test]
async fn system_prompt_is_a_single_token() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake_claude(tmp.path(), HAPPY_TAIL);
    let mut req = request("hi");
    req.system = "--verbose looks like a flag".into();
    let _ = collect(&bridge(bin), &req).await;

    let argv = std::fs::read_to_string(tmp.path().join("argv.txt")).unwrap();
    assert!(
        argv.lines()
            .any(|arg| arg == "--append-system-prompt=--verbose looks like a flag"),
        "{argv}"
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
async fn failed_result_subtype_includes_stderr() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake_claude(
        tmp.path(),
        r#"echo 'usage limit hit' >&2
echo '{"type":"result","subtype":"error_max_turns","usage":{}}'
"#,
    );
    let stream = bridge(bin).stream(&request("hi")).await.unwrap();
    let events: Vec<_> = stream.collect().await;
    match events.last() {
        Some(Err(ProviderError::Stream(message))) => {
            assert!(message.contains("error_max_turns"), "{message}");
            assert!(message.contains("usage limit hit"), "{message}");
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
    let bin = fake_claude(tmp.path(), HAPPY_TAIL);
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
    let _ = collect(&bridge(bin), &req).await;
    let stdin = std::fs::read_to_string(tmp.path().join("stdin.txt")).unwrap();
    assert_eq!(stdin, "User:\nfirst\n\nAssistant:\nreply\n\nUser:\nsecond");
}
