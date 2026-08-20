use futures::StreamExt;
use rushai_protocol::{Part, Role, TokenUsage};
use rushai_provider::{
    ChatMessage, ChatRequest, ModelInfo, OpenAiCompat, Provider, ProviderError, ProviderEvent,
    Retrying, StopReason, ToolDef,
};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn model() -> ModelInfo {
    ModelInfo {
        id: "gpt-5.2".into(),
        context_window: 400_000,
        max_output: 32_000,
    }
}

fn provider(server: &MockServer) -> OpenAiCompat {
    OpenAiCompat::new("test-key".into(), model(), format!("{}/v1", server.uri()))
}

fn sse_response(fixture: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_raw(fixture.as_bytes().to_vec(), "text/event-stream")
}

#[tokio::test]
async fn streams_text_reasoning_and_tool_calls() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(sse_response(include_str!(
            "fixtures/compat_text_tool_call.sse"
        )))
        .mount(&server)
        .await;

    let stream = provider(&server)
        .stream(&ChatRequest::default())
        .await
        .unwrap();
    let events: Vec<_> = stream
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();

    assert_eq!(
        events,
        vec![
            ProviderEvent::TextDelta("Sure, ".into()),
            ProviderEvent::TextDelta("listing.".into()),
            ProviderEvent::Reasoning {
                text: "user wants files".into(),
                signature: None,
            },
            ProviderEvent::ToolCallStart {
                id: "call_1".into(),
                name: "bash".into(),
            },
            ProviderEvent::ToolCallDelta {
                id: "call_1".into(),
                json: "{\"command\":".into(),
            },
            ProviderEvent::ToolCallDelta {
                id: "call_1".into(),
                json: "\"ls\"}".into(),
            },
            ProviderEvent::ToolCallEnd {
                id: "call_1".into()
            },
            ProviderEvent::Usage(TokenUsage {
                input: 30,
                output: 12,
                cache_read: 8,
                cache_write: 0,
            }),
            ProviderEvent::Done {
                stop: StopReason::ToolUse,
            },
        ]
    );
}

#[tokio::test]
async fn request_body_maps_parts_to_wire_messages() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse_response("data: [DONE]\n\n"))
        .mount(&server)
        .await;

    let request = ChatRequest {
        system: "be brief".into(),
        messages: vec![
            ChatMessage {
                role: Role::User,
                parts: vec![Part::Text {
                    text: "run ls".into(),
                }],
            },
            ChatMessage {
                role: Role::Assistant,
                parts: vec![
                    Part::Reasoning {
                        text: "dropped on replay".into(),
                        signature: None,
                    },
                    Part::Text { text: "ok".into() },
                    Part::ToolCall {
                        id: "call_1".into(),
                        name: "bash".into(),
                        input: json!({ "command": "ls" }),
                    },
                ],
            },
            ChatMessage {
                role: Role::User,
                parts: vec![Part::ToolResult {
                    id: "call_1".into(),
                    content: "Cargo.toml".into(),
                    is_error: false,
                }],
            },
        ],
        tools: vec![ToolDef {
            name: "bash".into(),
            description: "run a command".into(),
            input_schema: json!({ "type": "object" }),
        }],
        max_tokens: Some(512),
    };
    let stream = provider(&server).stream(&request).await.unwrap();
    let _: Vec<_> = stream.collect().await;

    let sent: Vec<Request> = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&sent[0].body).unwrap();

    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(body["max_completion_tokens"], 512);
    assert_eq!(body["tools"][0]["function"]["name"], "bash");

    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"], "run ls");
    // Assistant message: reasoning dropped, tool call attached.
    assert_eq!(messages[2]["role"], "assistant");
    assert_eq!(messages[2]["content"], "ok");
    assert_eq!(messages[2]["tool_calls"][0]["id"], "call_1");
    assert_eq!(
        messages[2]["tool_calls"][0]["function"]["arguments"],
        "{\"command\":\"ls\"}"
    );
    // Tool result fans out into its own role:tool message.
    assert_eq!(messages[3]["role"], "tool");
    assert_eq!(messages[3]["tool_call_id"], "call_1");
    assert_eq!(messages[3]["content"], "Cargo.toml");
}

#[tokio::test]
async fn http_error_is_reported_with_status() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).set_body_string("slow down"))
        .mount(&server)
        .await;

    let err = match provider(&server).stream(&ChatRequest::default()).await {
        Err(err) => err,
        Ok(_) => panic!("expected an error"),
    };
    assert!(matches!(err, ProviderError::Api { status: 429, .. }));
}

/// Live smoke test. Run with:
/// `OPENAI_API_KEY=... cargo test -p rushai-provider -- --ignored`
#[tokio::test]
#[ignore = "hits the real API, needs OPENAI_API_KEY"]
async fn live_stream_smoke() {
    let key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY not set");
    let provider = OpenAiCompat::new(
        key,
        ModelInfo {
            id: "gpt-5-mini".into(),
            context_window: 400_000,
            max_output: 1024,
        },
        "https://api.openai.com/v1".into(),
    );
    let stream = provider
        .stream(&ChatRequest {
            messages: vec![ChatMessage {
                role: Role::User,
                parts: vec![Part::Text {
                    text: "say hi".into(),
                }],
            }],
            max_tokens: Some(64),
            ..Default::default()
        })
        .await
        .unwrap();
    let events: Vec<_> = stream.collect().await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Ok(ProviderEvent::TextDelta(_))))
    );
}

#[tokio::test(start_paused = true)]
async fn retry_after_header_drives_the_backoff_delay() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "3")
                .set_body_string("try later"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(sse_response("data: [DONE]\n\n"))
        .mount(&server)
        .await;

    let retrying = Retrying::new(OpenAiCompat::with_client(
        reqwest::Client::new(),
        "test-key".into(),
        model(),
        format!("{}/v1", server.uri()),
    ));
    let started = tokio::time::Instant::now();
    let stream = retrying.stream(&ChatRequest::default()).await.unwrap();
    let _: Vec<_> = stream.collect().await;
    let elapsed = started.elapsed();
    // Lower bound only: in-process wiremock timers inflate the virtual clock.
    assert!(
        elapsed >= std::time::Duration::from_secs(3),
        "elapsed {elapsed:?} ignores Retry-After"
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}
