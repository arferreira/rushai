//! A scripted provider for tests and offline runs.

use async_stream::stream;

use crate::{
    ChatRequest, EventStream, ModelInfo, Provider, ProviderError, ProviderEvent, StopReason,
};

/// Replays a fixed event script. `Fault` entries become stream errors,
/// letting tests inject failures at arbitrary positions.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scripted {
    Event(ProviderEvent),
    Fault { message: String },
}

pub struct FakeProvider {
    model: ModelInfo,
    script: Vec<Scripted>,
}

impl FakeProvider {
    pub fn new(script: Vec<Scripted>) -> Self {
        Self {
            model: ModelInfo {
                id: "fake".into(),
                context_window: 200_000,
                max_output: 8192,
            },
            script,
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
        let script = self.script.clone();
        Ok(Box::pin(stream! {
            for entry in script {
                match entry {
                    Scripted::Event(event) => yield Ok(event),
                    Scripted::Fault { message } => {
                        yield Err(ProviderError::Stream(message));
                        return;
                    }
                }
            }
        }))
    }
}
