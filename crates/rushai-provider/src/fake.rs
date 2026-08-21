//! A scripted provider for tests and offline runs.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_stream::stream;

use crate::{
    ChatRequest, EventStream, ModelInfo, Provider, ProviderError, ProviderEvent, StopReason,
};

/// One scripted stream entry. `Fault` becomes a stream error, `Hang` never
/// resolves (for cancellation tests), `Panic` unwinds the run task (to test
/// that a panicking run still reports completion and never wedges its session).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scripted {
    Event(ProviderEvent),
    Fault { message: String },
    Hang,
    Panic,
}

/// Replays scripts turn by turn: each `stream()` call plays the next script,
/// and the last one repeats once exhausted.
pub struct FakeProvider {
    model: ModelInfo,
    turns: Mutex<VecDeque<Vec<Scripted>>>,
    last: Vec<Scripted>,
}

impl FakeProvider {
    pub fn new(script: Vec<Scripted>) -> Self {
        Self::turns(vec![script])
    }

    pub fn turns(scripts: Vec<Vec<Scripted>>) -> Self {
        let last = scripts.last().cloned().unwrap_or_default();
        Self {
            model: ModelInfo {
                id: "fake".into(),
                context_window: 200_000,
                max_output: 8192,
                cost: None,
            },
            turns: Mutex::new(scripts.into_iter().collect()),
            last,
        }
    }

    pub fn from_events(events: Vec<ProviderEvent>) -> Self {
        Self::new(events.into_iter().map(Scripted::Event).collect())
    }

    /// A minimal script: one text delta, then a clean finish.
    pub fn saying(text: &str) -> Self {
        Self::from_events(vec![
            ProviderEvent::TextDelta(text.to_owned()),
            ProviderEvent::Done {
                stop: StopReason::EndTurn,
            },
        ])
    }
}

#[async_trait::async_trait]
impl Provider for FakeProvider {
    fn model(&self) -> &ModelInfo {
        &self.model
    }

    async fn stream(&self, _request: &ChatRequest) -> Result<EventStream, ProviderError> {
        let script = self
            .turns
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| self.last.clone());
        Ok(Box::pin(stream! {
            for entry in script {
                match entry {
                    Scripted::Event(event) => yield Ok(event),
                    Scripted::Fault { message } => {
                        yield Err(ProviderError::Stream(message));
                        return;
                    }
                    Scripted::Hang => std::future::pending::<()>().await,
                    Scripted::Panic => panic!("scripted panic from FakeProvider"),
                }
            }
        }))
    }
}
