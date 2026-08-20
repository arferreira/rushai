use futures::StreamExt;
use rushai_protocol::{Part, Role, TokenUsage};
use rushai_provider::{
    Anthropic, ChatMessage, ChatRequest, ModelInfo, Provider, ProviderError, ProviderEvent,
    StopReason, ToolDef,
};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn model() -> ModelInfo {
    ModelInfo {
        id: "claude-opus-5".into(),
        context_window: 200_000,
        max_output: 8192,
        cost: None,
    }
}

fn sse_response(fixture: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_raw(fixture.as_bytes().to_vec(), "text/event-stream")
}

async fn collect(
    server: &MockServer,
    request: ChatRequest,
) -> Vec<Result<ProviderEvent, ProviderError>> {
    let provider = Anthropic::with_base_url("test-key".into(), model(), server.uri());
    let stream = provider.stream(&request).await.unwrap();
    stream.collect().await
}

#[tokio::test]
async fn streams_text_reasoning_and_tool_calls() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-key"))
        .respond_with(sse_response(include_str!("fixtures/text_tool_call.sse")))
        .mount(&server)
        .await;

    let events: Vec<_> = collect(&server, ChatRequest::default())
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();

    assert_eq!(
        events,
        vec![
            ProviderEvent::Usage(TokenUsage {
                input: 25,
                output: 0,
                cache_read: 5,
                cache_write: 10,
            }),
            ProviderEvent::Reasoning {
                text: "Let me look.".into(),
                signature: None,
            },
            ProviderEvent::Reasoning {
                text: String::new(),
                signature: Some("sig123".into()),
            },
            ProviderEvent::TextDelta("I'll list ".into()),
            ProviderEvent::TextDelta("the files.".into()),
            ProviderEvent::ToolCallStart {
                id: "toolu_1".into(),
                name: "bash".into(),
            },
            ProviderEvent::ToolCallDelta {
                id: "toolu_1".into(),
                json: "{\"command\":".into(),
            },
            ProviderEvent::ToolCallDelta {
                id: "toolu_1".into(),
                json: "\"ls\"}".into(),
            },
            ProviderEvent::ToolCallEnd {
                id: "toolu_1".into()
            },
            ProviderEvent::Usage(TokenUsage {
                input: 0,
                output: 40,
                cache_read: 0,
                cache_write: 0,
            }),
            ProviderEvent::Done {
                stop: StopReason::ToolUse
            },
        ]
    );
}

#[tokio::test]
async fn mid_stream_error_surfaces_as_err() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(include_str!("fixtures/mid_stream_error.sse")))
        .mount(&server)
        .await;

    let events = collect(&server, ChatRequest::default()).await;
    assert!(matches!(
        events.last(),
        Some(Err(ProviderError::Api { message, .. })) if message == "Overloaded"
    ));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Ok(ProviderEvent::TextDelta(t)) if t == "partial"))
    );
}

#[tokio::test]
async fn http_error_is_reported_with_status() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
        .mount(&server)
        .await;

    let provider = Anthropic::with_base_url("test-key".into(), model(), server.uri());
    let err = match provider.stream(&ChatRequest::default()).await {
        Err(err) => err,
        Ok(_) => panic!("expected an error"),
    };
    assert!(matches!(err, ProviderError::Api { status: 401, .. }));
}

#[tokio::test]
async fn request_body_maps_parts_and_places_cache_breakpoints() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse_response(include_str!("fixtures/mid_stream_error.sse")))
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
                        text: "checking".into(),
                        signature: Some("sig".into()),
                    },
                    Part::ToolCall {
                        id: "toolu_1".into(),
                        name: "bash".into(),
                        input: json!({ "command": "ls" }),
                    },
                ],
            },
            ChatMessage {
                role: Role::User,
                parts: vec![Part::ToolResult {
                    id: "toolu_1".into(),
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
        max_tokens: None,
    };
    let _ = collect(&server, request).await;

    let sent: Vec<Request> = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&sent[0].body).unwrap();

    assert_eq!(body["model"], "claude-opus-5");
    assert_eq!(body["max_tokens"], 8192);
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(body["tools"][0]["cache_control"]["type"], "ephemeral");

    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "user");
    // Only the last two messages carry cache breakpoints.
    assert!(messages[0]["content"][0]["cache_control"].is_null());
    assert_eq!(
        messages[1]["content"][1]["cache_control"]["type"],
        "ephemeral"
    );
    assert_eq!(
        messages[2]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );

    assert_eq!(messages[1]["content"][0]["type"], "thinking");
    assert_eq!(messages[1]["content"][0]["signature"], "sig");
    assert_eq!(messages[1]["content"][1]["type"], "tool_use");
    assert_eq!(messages[2]["content"][0]["type"], "tool_result");
    assert_eq!(messages[2]["content"][0]["tool_use_id"], "toolu_1");
}

#[tokio::test]
async fn explicit_max_tokens_wins_over_catalog_max_output() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse_response(include_str!("fixtures/mid_stream_error.sse")))
        .mount(&server)
        .await;

    let model = rushai_provider::catalog::lookup("anthropic", "sonnet").unwrap();
    assert_eq!(model.max_output, 128_000);
    let provider = Anthropic::with_base_url("test-key".into(), model, server.uri());

    let request = ChatRequest {
        max_tokens: Some(8192),
        ..Default::default()
    };
    let _ = provider
        .stream(&request)
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    // Without an explicit value the model's max_output is the fallback.
    let request = ChatRequest::default();
    let _ = provider
        .stream(&request)
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;

    let sent: Vec<Request> = server.received_requests().await.unwrap();
    let first: Value = serde_json::from_slice(&sent[0].body).unwrap();
    let second: Value = serde_json::from_slice(&sent[1].body).unwrap();
    assert_eq!(first["max_tokens"], 8192);
    assert_eq!(second["max_tokens"], 128_000);
}

/// Live smoke test. Run with:
/// `ANTHROPIC_API_KEY=... cargo test -p rushai-provider -- --ignored`
#[tokio::test]
#[ignore = "hits the real API, needs ANTHROPIC_API_KEY"]
async fn live_stream_smoke() {
    let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY not set");
    let provider = Anthropic::new(
        key,
        ModelInfo {
            id: "claude-haiku-4-5-20251001".into(),
            context_window: 200_000,
            max_output: 1024,
            cost: None,
        },
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
    assert!(matches!(
        events.last(),
        Some(Ok(ProviderEvent::Done { .. }))
    ));
}
