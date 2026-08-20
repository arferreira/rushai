use std::time::Duration;

use rand::Rng;
use reqwest::header::HeaderMap;

use crate::{ChatRequest, EventStream, ModelInfo, Provider, ProviderError};

const BASE: Duration = Duration::from_millis(500);
const CAP: Duration = Duration::from_secs(30);
const MAX_ATTEMPTS: u32 = 5;

/// Retries the initial request with full-jitter exponential backoff.
/// Mid-stream failures pass through; recovering those needs message
/// state and belongs to the agent loop.
pub struct Retrying<P> {
    inner: P,
}

impl<P> Retrying<P> {
    pub fn new(inner: P) -> Self {
        Self { inner }
    }
}

#[async_trait::async_trait]
impl<P: Provider> Provider for Retrying<P> {
    fn model(&self) -> &ModelInfo {
        self.inner.model()
    }

    async fn stream(&self, request: &ChatRequest) -> Result<EventStream, ProviderError> {
        let mut attempt = 0;
        loop {
            match self.inner.stream(request).await {
                Ok(stream) => return Ok(stream),
                Err(err) if attempt + 1 < MAX_ATTEMPTS && retryable(&err) => {
                    tokio::time::sleep(delay(&err, attempt)).await;
                    attempt += 1;
                }
                Err(err) => return Err(err),
            }
        }
    }
}

fn retryable(err: &ProviderError) -> bool {
    match err {
        ProviderError::Http(err) => err.is_connect() || err.is_timeout(),
        ProviderError::Api { status, .. } => {
            matches!(status, 408 | 429) || (500..=599).contains(status)
        }
        _ => false,
    }
}

fn delay(err: &ProviderError, attempt: u32) -> Duration {
    if let ProviderError::Api {
        retry_after: Some(after),
        ..
    } = err
    {
        // Small jitter on top so a herd told "retry in 3s" spreads out.
        let base = (*after).min(CAP);
        return base + rand::rng().random_range(Duration::ZERO..=base / 4);
    }
    let ceiling = BASE
        .checked_mul(2u32.saturating_pow(attempt))
        .unwrap_or(CAP)
        .min(CAP);
    rand::rng().random_range(Duration::ZERO..=ceiling)
}

/// Seconds form only; the HTTP-date form is rare enough to ignore.
pub(crate) fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get("retry-after")?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::{ProviderEvent, StopReason};

    struct Flaky {
        model: ModelInfo,
        attempts: AtomicU32,
        failures: u32,
        error: fn() -> ProviderError,
    }

    impl Flaky {
        fn new(failures: u32, error: fn() -> ProviderError) -> Self {
            Self {
                model: ModelInfo {
                    id: "flaky".into(),
                    context_window: 1,
                    max_output: 1,
                },
                attempts: AtomicU32::new(0),
                failures,
                error,
            }
        }
    }

    #[async_trait::async_trait]
    impl Provider for Flaky {
        fn model(&self) -> &ModelInfo {
            &self.model
        }

        async fn stream(&self, _request: &ChatRequest) -> Result<EventStream, ProviderError> {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
            if attempt < self.failures {
                return Err((self.error)());
            }
            Ok(Box::pin(futures::stream::iter([Ok(ProviderEvent::Done {
                stop: StopReason::EndTurn,
            })])))
        }
    }

    fn overloaded() -> ProviderError {
        ProviderError::Api {
            status: 429,
            message: "slow down".into(),
            retry_after: None,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn retries_429_until_success() {
        let provider = Retrying::new(Flaky::new(2, overloaded));
        assert!(provider.stream(&ChatRequest::default()).await.is_ok());
        assert_eq!(provider.inner.attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn gives_up_after_max_attempts() {
        let provider = Retrying::new(Flaky::new(u32::MAX, overloaded));
        assert!(provider.stream(&ChatRequest::default()).await.is_err());
        assert_eq!(provider.inner.attempts.load(Ordering::SeqCst), 5);
    }

    #[tokio::test(start_paused = true)]
    async fn auth_errors_do_not_retry() {
        let provider = Retrying::new(Flaky::new(u32::MAX, || ProviderError::Api {
            status: 401,
            message: "bad key".into(),
            retry_after: None,
        }));
        assert!(provider.stream(&ChatRequest::default()).await.is_err());
        assert_eq!(provider.inner.attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn honors_retry_after() {
        let provider = Retrying::new(Flaky::new(1, || ProviderError::Api {
            status: 429,
            message: "later".into(),
            retry_after: Some(Duration::from_secs(7)),
        }));
        let started = tokio::time::Instant::now();
        assert!(provider.stream(&ChatRequest::default()).await.is_ok());
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_secs(7) && elapsed <= Duration::from_millis(8750),
            "elapsed {elapsed:?} outside retry-after + jitter window"
        );
    }
}
