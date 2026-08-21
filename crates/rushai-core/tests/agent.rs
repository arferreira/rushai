use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rushai_core::agent::{Engine, EngineConfig};
use rushai_core::permission::PermissionSpec;
use rushai_core::store::Store;
use rushai_core::tool::{RunToken, Tool, ToolCtx, ToolError};
use rushai_protocol::{Decision, Event, FinishReason, Op, Part, Role, SessionId, UserPart};
use rushai_provider::fake::{FakeProvider, Scripted};
use rushai_provider::{ProviderEvent, StopReason, ToolDef};
use serde_json::{Value, json};
use tokio::sync::mpsc;

struct Echo;

#[async_trait::async_trait]
impl Tool for Echo {
    fn name(&self) -> &'static str {
        "echo"
    }
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "echo".into(),
            description: "echo the msg field".into(),
            input_schema: json!({ "type": "object" }),
        }
    }
    fn permission(&self, _input: &Value) -> Option<PermissionSpec> {
        None
    }
    async fn run(&self, _ctx: &ToolCtx, input: Value, _run: RunToken) -> Result<String, ToolError> {
        Ok(format!(
            "echo:{}",
            input["msg"].as_str().unwrap_or_default()
        ))
    }
}

struct Gated {
    ran: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl Tool for Gated {
    fn name(&self) -> &'static str {
        "gated"
    }
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "gated".into(),
            description: "needs a grant".into(),
            input_schema: json!({ "type": "object" }),
        }
    }
    fn permission(&self, _input: &Value) -> Option<PermissionSpec> {
        Some(PermissionSpec {
            tool: "gated".into(),
            action: "poke".into(),
            path: None,
            description: "poke the thing".into(),
        })
    }
    async fn run(
        &self,
        _ctx: &ToolCtx,
        _input: Value,
        _run: RunToken,
    ) -> Result<String, ToolError> {
        self.ran.store(true, Ordering::SeqCst);
        Ok("poked".into())
    }
}

fn tool_call_turn(id: &str, name: &str, input: &str) -> Vec<Scripted> {
    vec![
        Scripted::Event(ProviderEvent::ToolCallStart {
            id: id.into(),
            name: name.into(),
        }),
        Scripted::Event(ProviderEvent::ToolCallDelta {
            id: id.into(),
            json: input.into(),
        }),
        Scripted::Event(ProviderEvent::ToolCallEnd { id: id.into() }),
        Scripted::Event(ProviderEvent::Done {
            stop: StopReason::ToolUse,
        }),
    ]
}

fn text_turn(text: &str) -> Vec<Scripted> {
    vec![
        Scripted::Event(ProviderEvent::TextDelta(text.into())),
        Scripted::Event(ProviderEvent::Done {
            stop: StopReason::EndTurn,
        }),
    ]
}

async fn engine(
    provider: FakeProvider,
    tools: Vec<Arc<dyn Tool>>,
) -> (Engine, mpsc::Receiver<Event>, Store, SessionId) {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session("test".into(), None).await.unwrap().id;
    let (engine, events) = Engine::spawn(EngineConfig {
        store: store.clone(),
        provider: Arc::new(provider),
        tools,
        cwd: std::env::current_dir().unwrap(),
        yolo: false,
        max_tokens: Some(1024),
    });
    (engine, events, store, session)
}

fn prompt(engine: &Engine, session: &SessionId, text: &str) {
    assert!(engine.submit(Op::Prompt {
        session: session.clone(),
        parts: vec![UserPart::Text { text: text.into() }],
    }));
}

/// Collect events until (and including) the next RunFinished.
async fn collect_run(events: &mut mpsc::Receiver<Event>) -> Vec<Event> {
    let mut collected = Vec::new();
    while let Some(event) = events.recv().await {
        let done = matches!(event, Event::RunFinished { .. });
        collected.push(event);
        if done {
            return collected;
        }
    }
    panic!("event channel closed before RunFinished");
}

fn finish_reason(events: &[Event]) -> FinishReason {
    match events.last() {
        Some(Event::RunFinished { reason, .. }) => *reason,
        other => panic!("expected RunFinished, got {other:?}"),
    }
}

#[tokio::test]
async fn multi_turn_tool_loop_persists_results() {
    let provider = FakeProvider::turns(vec![
        tool_call_turn("c1", "echo", r#"{"msg":"hi"}"#),
        text_turn("done"),
    ]);
    let (engine, mut events, store, session) = engine(provider, vec![Arc::new(Echo)]).await;
    prompt(&engine, &session, "call echo");

    let run = collect_run(&mut events).await;
    assert_eq!(finish_reason(&run), FinishReason::EndTurn);
    assert!(run.iter().any(|e| matches!(
        e,
        Event::ToolDone { output, is_error: false, .. } if output == "echo:hi"
    )));

    let messages = store.messages(&session).await.unwrap();
    let roles: Vec<Role> = messages.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        [Role::User, Role::Assistant, Role::User, Role::Assistant]
    );
    assert!(matches!(
        &messages[1].parts[..],
        [
            Part::ToolCall { name, input, .. },
            Part::Finish { reason: FinishReason::ToolUse, .. },
        ] if name == "echo" && input == &json!({"msg": "hi"})
    ));
    assert!(matches!(
        &messages[2].parts[..],
        [Part::ToolResult { content, is_error: false, .. }] if content == "echo:hi"
    ));
    assert!(matches!(
        &messages[3].parts[..],
        [
            Part::Text { text },
            Part::Finish { reason: FinishReason::EndTurn, .. },
        ] if text == "done"
    ));
}

#[tokio::test]
async fn cancel_mid_stream_finalizes_the_partial_message() {
    let provider = FakeProvider::new(vec![
        Scripted::Event(ProviderEvent::TextDelta("partial".into())),
        Scripted::Hang,
    ]);
    let (engine, mut events, store, session) = engine(provider, vec![]).await;
    prompt(&engine, &session, "go");

    // Wait for the delta so the cancel lands mid-stream.
    loop {
        match events.recv().await.expect("events open") {
            Event::PartDelta { .. } => break,
            _ => continue,
        }
    }
    engine.submit(Op::Cancel {
        session: session.clone(),
    });
    let run = collect_run(&mut events).await;
    assert_eq!(finish_reason(&run), FinishReason::Canceled);

    let messages = store.messages(&session).await.unwrap();
    let assistant = messages.last().unwrap();
    assert!(matches!(
        &assistant.parts[..],
        [
            Part::Text { text },
            Part::Finish { reason: FinishReason::Canceled, .. },
        ] if text == "partial"
    ));

    // Nothing else arrives after RunFinished.
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert!(matches!(
        events.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn cancel_drops_queued_prompts() {
    // FakeProvider hands out scripts in stream() call order, so the first run
    // must actually reach the provider (proven by its MessageStarted) before
    // the second is queued. Otherwise which run consumes the hang is a race.
    let provider = FakeProvider::turns(vec![vec![Scripted::Hang], text_turn("alive")]);
    let (engine, mut events, _store, session) = engine(provider, vec![]).await;
    prompt(&engine, &session, "first");
    wait_for(&mut events, |e| matches!(e, Event::MessageStarted { .. })).await;

    prompt(&engine, &session, "second");
    wait_for(&mut events, |e| {
        matches!(e, Event::RunAccepted { seq: 2, .. })
    })
    .await;
    engine.submit(Op::Cancel {
        session: session.clone(),
    });

    // Both accepted runs finish exactly once, both canceled: the queued one
    // never starts, the active one is interrupted.
    let mut finished = Vec::new();
    while finished.len() < 2 {
        match events.recv().await {
            Some(Event::RunFinished { seq, reason, .. }) => finished.push((seq, reason)),
            Some(_) => {}
            None => panic!("channel closed"),
        }
    }
    assert!(finished.iter().all(|(_, r)| *r == FinishReason::Canceled));
    let seqs: Vec<u64> = finished.iter().map(|(s, _)| *s).collect();
    assert_eq!(
        seqs,
        [2, 1],
        "queued seq2 drops before the active seq1 unwinds"
    );

    // The engine still serves new prompts, and the queued run never consumed
    // its script, so "alive" is still waiting.
    prompt(&engine, &session, "third");
    let run = collect_run(&mut events).await;
    assert_eq!(finish_reason(&run), FinishReason::EndTurn);
}

async fn wait_for(events: &mut mpsc::Receiver<Event>, pred: impl Fn(&Event) -> bool) {
    while let Some(event) = events.recv().await {
        if pred(&event) {
            return;
        }
    }
    panic!("event channel closed before the awaited event");
}

#[tokio::test]
async fn permission_decision_grants_and_runs_the_tool() {
    let ran = Arc::new(AtomicBool::new(false));
    let provider = FakeProvider::turns(vec![
        tool_call_turn("c1", "gated", "{}"),
        text_turn("after"),
    ]);
    let (engine, mut events, _store, session) =
        engine(provider, vec![Arc::new(Gated { ran: ran.clone() })]).await;
    prompt(&engine, &session, "poke");

    let run = drive_permissions(&engine, &mut events, Decision::Once).await;
    assert_eq!(finish_reason(&run), FinishReason::EndTurn);
    assert!(ran.load(Ordering::SeqCst));
    assert!(run.iter().any(|e| matches!(
        e,
        Event::ToolDone { output, is_error: false, .. } if output == "poked"
    )));
}

#[tokio::test]
async fn permission_deny_blocks_the_tool_but_not_the_run() {
    let ran = Arc::new(AtomicBool::new(false));
    let provider = FakeProvider::turns(vec![
        tool_call_turn("c1", "gated", "{}"),
        text_turn("after"),
    ]);
    let (engine, mut events, store, session) =
        engine(provider, vec![Arc::new(Gated { ran: ran.clone() })]).await;
    prompt(&engine, &session, "poke");

    let run = drive_permissions(&engine, &mut events, Decision::Deny).await;
    // The deny becomes an error tool result and the run continues.
    assert_eq!(finish_reason(&run), FinishReason::EndTurn);
    assert!(!ran.load(Ordering::SeqCst));

    let messages = store.messages(&session).await.unwrap();
    assert!(messages.iter().any(|m| {
        matches!(
            &m.parts[..],
            [Part::ToolResult { content, is_error: true, .. }]
                if content.contains("permission denied")
        )
    }));
}

/// Answer every permission request with `decision`, collecting until the run
/// finishes.
async fn drive_permissions(
    engine: &Engine,
    events: &mut mpsc::Receiver<Event>,
    decision: Decision,
) -> Vec<Event> {
    let mut collected = Vec::new();
    while let Some(event) = events.recv().await {
        if let Event::PermissionRequested { request } = &event {
            engine.submit(Op::PermissionDecision {
                request: request.id.clone(),
                decision,
            });
        }
        let done = matches!(event, Event::RunFinished { .. });
        collected.push(event);
        if done {
            return collected;
        }
    }
    panic!("event channel closed before RunFinished");
}

#[tokio::test]
async fn unknown_tool_reports_an_error_result() {
    let provider =
        FakeProvider::turns(vec![tool_call_turn("c1", "nope", "{}"), text_turn("after")]);
    let (engine, mut events, store, session) = engine(provider, vec![Arc::new(Echo)]).await;
    prompt(&engine, &session, "call it");

    let run = collect_run(&mut events).await;
    assert_eq!(finish_reason(&run), FinishReason::EndTurn);
    let messages = store.messages(&session).await.unwrap();
    assert!(messages.iter().any(|m| {
        matches!(
            &m.parts[..],
            [Part::ToolResult { content, is_error: true, .. }]
                if content.contains("no such tool")
        )
    }));
}

#[tokio::test]
async fn loop_guard_trips_at_the_turn_bound() {
    // The single script repeats forever: every turn asks for the tool again.
    let provider = FakeProvider::new(tool_call_turn("c1", "echo", r#"{"msg":"again"}"#));
    let (engine, mut events, _store, session) = engine(provider, vec![Arc::new(Echo)]).await;
    prompt(&engine, &session, "loop");

    let run = collect_run(&mut events).await;
    assert_eq!(finish_reason(&run), FinishReason::Error);
    assert!(run.iter().any(|e| matches!(
        e,
        Event::Error { message } if message.contains("50")
    )));
}

#[tokio::test]
async fn provider_fault_yields_exactly_one_run_finished() {
    let provider = FakeProvider::new(vec![
        Scripted::Event(ProviderEvent::TextDelta("partial".into())),
        Scripted::Fault {
            message: "connection reset".into(),
        },
    ]);
    let (engine, mut events, store, session) = engine(provider, vec![]).await;
    prompt(&engine, &session, "go");

    let run = collect_run(&mut events).await;
    assert_eq!(finish_reason(&run), FinishReason::Error);
    assert!(run.iter().any(|e| matches!(
        e,
        Event::Error { message } if message.contains("connection reset")
    )));
    let finishes = run
        .iter()
        .filter(|e| matches!(e, Event::RunFinished { .. }))
        .count();
    assert_eq!(finishes, 1);

    // The partial message was finalized as an error, not left dangling.
    let messages = store.messages(&session).await.unwrap();
    let assistant = messages.last().unwrap();
    assert!(matches!(
        assistant.parts.last(),
        Some(Part::Finish {
            reason: FinishReason::Error,
            ..
        })
    ));

    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert!(matches!(
        events.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
}
