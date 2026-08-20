use futures::StreamExt;
use rushai_protocol::{Part, Role};
use rushai_provider::{
    ChatMessage, ChatRequest, Copilot, CopilotAuth, ModelInfo, Provider, ProviderEvent,
};
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn model() -> ModelInfo {
    ModelInfo {
        id: "gpt-5.2".into(),
        context_window: 128_000,
        max_output: 16_000,
    }
}

fn far_future() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 1800
}

#[tokio::test(start_paused = true)]
async fn device_flow_handles_pending_and_slow_down() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/login/device/code"))
        .and(body_string_contains("client_id=Iv1.b507a08c87ecfe98"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "device_code": "dev123",
            "user_code": "ABCD-1234",
            "verification_uri": "https://github.com/login/device",
            "interval": 1,
            "expires_in": 900,
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "error": "authorization_pending"
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "error": "slow_down" })),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .and(body_string_contains(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "ghu_secret"
        })))
        .mount(&server)
        .await;

    let auth = CopilotAuth::with_client(reqwest::Client::new(), server.uri(), server.uri());
    let code = auth.start().await.unwrap();
    assert_eq!(code.user_code, "ABCD-1234");
    let started = tokio::time::Instant::now();
    let token = auth.poll(&code).await.unwrap();
    assert_eq!(token, "ghu_secret");
    // 1s (pending) + 1s (slow_down) + 6s (grown interval). Lower bound only:
    // wiremock's hyper timers also feed tokio's auto-advance, inflating the
    // virtual clock unpredictably above the sleeps we control.
    assert!(
        started.elapsed() >= std::time::Duration::from_secs(8),
        "interval did not grow after slow_down: {:?}",
        started.elapsed()
    );
    // pending + slow_down + success
    let polls = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/login/oauth/access_token")
        .count();
    assert_eq!(polls, 3);
}

#[tokio::test]
async fn chat_sends_required_headers_and_refreshes_token() {
    let server = MockServer::start().await;
    // First tk_ token is already expired, forcing a refresh on second call.
    let expired = ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "token": "tk_old",
        "expires_at": 1,
    }));
    let fresh = ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "token": "tk_fresh",
        "expires_at": far_future(),
    }));
    Mock::given(method("GET"))
        .and(path("/copilot_internal/v2/token"))
        .and(header("authorization", "token ghu_secret"))
        .respond_with(expired)
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/copilot_internal/v2/token"))
        .respond_with(fresh)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("copilot-integration-id", "vscode-chat"))
        .and(header("editor-version", "vscode/1.99.0"))
        .and(header("user-agent", "GitHubCopilotChat/0.26.0"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(
                    b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n\
                      data: [DONE]\n\n"
                        .to_vec(),
                    "text/event-stream",
                ),
        )
        .mount(&server)
        .await;

    let provider = Copilot::with_base_urls(
        "ghu_secret".into(),
        model(),
        CopilotAuth::with_base_urls(server.uri(), server.uri()),
        server.uri(),
    );
    let request = ChatRequest {
        messages: vec![ChatMessage {
            role: Role::User,
            parts: vec![Part::Text { text: "hi".into() }],
        }],
        ..Default::default()
    };

    for _ in 0..2 {
        let stream = provider.stream(&request).await.unwrap();
        let events: Vec<_> = stream.collect::<Vec<_>>().await;
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Ok(ProviderEvent::TextDelta(t)) if t == "hi"))
        );
    }

    let requests: Vec<Request> = server.received_requests().await.unwrap();
    let exchanges = requests
        .iter()
        .filter(|r| r.url.path() == "/copilot_internal/v2/token")
        .count();
    // tk_old expired immediately, so the second chat call re-exchanged.
    assert_eq!(exchanges, 2);
    let chat = requests
        .iter()
        .rfind(|r| r.url.path() == "/chat/completions")
        .unwrap();
    assert_eq!(
        chat.headers.get("authorization").unwrap(),
        "Bearer tk_fresh"
    );
}

#[tokio::test(start_paused = true)]
async fn poll_gives_up_when_the_code_expires() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "error": "authorization_pending"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/login/device/code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "device_code": "dev123",
            "user_code": "ABCD-1234",
            "verification_uri": "https://github.com/login/device",
            "interval": 1,
            "expires_in": 2,
        })))
        .mount(&server)
        .await;

    let auth = CopilotAuth::with_client(reqwest::Client::new(), server.uri(), server.uri());
    let code = auth.start().await.unwrap();
    let err = auth.poll(&code).await.unwrap_err();
    assert!(err.to_string().contains("expired"), "{err}");
}

#[tokio::test]
async fn chat_401_forces_one_reexchange() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/copilot_internal/v2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "token": "tk_fresh",
            "expires_at": far_future(),
        })))
        .mount(&server)
        .await;
    // First chat call rejects the token; the provider must re-exchange and retry once.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_string("bad token"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(
                    b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"}}]}\n\n\
                      data: [DONE]\n\n"
                        .to_vec(),
                    "text/event-stream",
                ),
        )
        .mount(&server)
        .await;

    let provider = Copilot::with_base_urls(
        "ghu_secret".into(),
        model(),
        CopilotAuth::with_base_urls(server.uri(), server.uri()),
        server.uri(),
    );
    let stream = provider.stream(&ChatRequest::default()).await.unwrap();
    let events: Vec<_> = stream.collect::<Vec<_>>().await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Ok(ProviderEvent::TextDelta(t)) if t == "ok"))
    );

    let requests: Vec<Request> = server.received_requests().await.unwrap();
    let exchanges = requests
        .iter()
        .filter(|r| r.url.path() == "/copilot_internal/v2/token")
        .count();
    assert_eq!(exchanges, 2);
}
