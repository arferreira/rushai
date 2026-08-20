use rushai_protocol::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;

fn roundtrip<T>(value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let text = serde_json::to_string(value).unwrap();
    let back: T = serde_json::from_str(&text).unwrap();
    assert_eq!(*value, back);
}

// The JSON shape is the contract between engine and frontends. These goldens
// fail if a rename, retag, or representation change alters the wire format.

#[test]
fn op_prompt_wire_format() {
    let op = Op::Prompt {
        session: SessionId::from("s1"),
        parts: vec![UserPart::Text { text: "hi".into() }],
    };
    assert_eq!(
        serde_json::to_value(&op).unwrap(),
        json!({
            "type": "prompt",
            "session": "s1",
            "parts": [{ "type": "text", "text": "hi" }]
        })
    );
}

#[test]
fn op_permission_decision_wire_format() {
    let op = Op::PermissionDecision {
        request: RequestId::from("r1"),
        decision: Decision::Session,
    };
    assert_eq!(
        serde_json::to_value(&op).unwrap(),
        json!({
            "type": "permission_decision",
            "request": "r1",
            "decision": "session"
        })
    );
}

#[test]
fn event_part_delta_wire_format() {
    let event = Event::PartDelta {
        message: MessageId::from("m1"),
        index: 0,
        delta: PartDelta::Text {
            delta: "tok".into(),
        },
    };
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "type": "part_delta",
            "message": "m1",
            "index": 0,
            "delta": { "type": "text", "delta": "tok" }
        })
    );
}

#[test]
fn reasoning_part_omits_missing_signature() {
    let part = Part::Reasoning {
        text: "thinking".into(),
        signature: None,
    };
    assert_eq!(
        serde_json::to_value(&part).unwrap(),
        json!({ "type": "reasoning", "text": "thinking" })
    );
    // And parses back without the field present.
    let back: Part =
        serde_json::from_value(json!({ "type": "reasoning", "text": "thinking" })).unwrap();
    assert_eq!(part, back);
}

#[test]
fn finish_part_wire_format() {
    let part = Part::Finish {
        reason: FinishReason::EndTurn,
        usage: TokenUsage {
            input: 10,
            output: 5,
            cache_read: 3,
            cache_write: 2,
        },
    };
    assert_eq!(
        serde_json::to_value(&part).unwrap(),
        json!({
            "type": "finish",
            "reason": "end_turn",
            "usage": { "input": 10, "output": 5, "cache_read": 3, "cache_write": 2 }
        })
    );
}

#[test]
fn representative_values_roundtrip() {
    roundtrip(&Op::Cancel {
        session: SessionId::from("s1"),
    });
    roundtrip(&Op::Shutdown);
    roundtrip(&Event::RunAccepted {
        session: SessionId::from("s1"),
        seq: 7,
    });
    roundtrip(&Event::ToolStarted {
        call: CallId::from("c1"),
        name: "bash".into(),
        input: json!({ "command": "ls" }),
    });
    roundtrip(&Event::PermissionRequested {
        request: PermissionRequest {
            id: RequestId::from("r1"),
            session: SessionId::from("s1"),
            tool: "edit".into(),
            action: "write".into(),
            path: Some("src/main.rs".into()),
            description: "Edit src/main.rs".into(),
        },
    });
    roundtrip(&Event::RunFinished {
        session: SessionId::from("s1"),
        seq: 7,
        reason: FinishReason::Canceled,
    });
    roundtrip(&Part::ToolCall {
        id: CallId::from("c1"),
        name: "grep".into(),
        input: json!({ "pattern": "fn main" }),
    });
    roundtrip(&Part::ToolResult {
        id: CallId::from("c1"),
        content: "src/main.rs:1".into(),
        is_error: false,
    });
}

#[test]
fn generated_ids_are_unique() {
    let a = SessionId::new();
    let b = SessionId::new();
    assert_ne!(a, b);
    assert!(!a.as_str().is_empty());
}
