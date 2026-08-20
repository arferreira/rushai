use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use rushai_protocol::{Decision, PermissionRequest, RequestId, SessionId};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

use crate::store::{Store, StoreError};

/// What a tool wants to do; the grant key is {tool, action, path}.
///
/// Paths must be absolute by the time they reach the service (dispatch
/// resolves them against the working directory). Grants match exactly;
/// prefix matching comes with the tools that need it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionSpec {
    pub tool: String,
    pub action: String,
    pub path: Option<String>,
    pub description: String,
}

#[derive(Debug, Error)]
pub enum PermissionError {
    #[error("permission denied: {0}")]
    Denied(String),
    #[error("permission channel closed")]
    Closed,
    #[error(transparent)]
    Store(#[from] StoreError),
}

type GrantKey = (String, String, Option<String>);
type SessionKey = (SessionId, GrantKey);

/// Checks grants and asks the frontend when none exist.
/// Requests go out on the channel returned by [`PermissionService::new`];
/// whoever owns the receiver answers via [`PermissionService::resolve`].
/// Concurrent asks for the same key share one request.
pub struct PermissionService {
    yolo: bool,
    store: Store,
    request_tx: mpsc::UnboundedSender<PermissionRequest>,
    next_waiter: AtomicU64,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    session_grants: HashSet<SessionKey>,
    inflight: HashMap<SessionKey, RequestId>,
    pending: HashMap<RequestId, Pending>,
}

struct Pending {
    key: SessionKey,
    waiters: HashMap<u64, oneshot::Sender<Decision>>,
}

impl PermissionService {
    pub fn new(yolo: bool, store: Store) -> (Self, mpsc::UnboundedReceiver<PermissionRequest>) {
        let (request_tx, request_rx) = mpsc::unbounded_channel();
        (
            Self {
                yolo,
                store,
                request_tx,
                next_waiter: AtomicU64::new(0),
                inner: Mutex::new(Inner::default()),
            },
            request_rx,
        )
    }

    pub async fn ensure(
        &self,
        session: &SessionId,
        spec: &PermissionSpec,
    ) -> Result<(), PermissionError> {
        if self.yolo {
            return Ok(());
        }
        let key: SessionKey = (
            session.clone(),
            (spec.tool.clone(), spec.action.clone(), spec.path.clone()),
        );
        // Coalesce before touching the store: if this key is already being
        // asked about, attach to that request instead of racing it.
        let wid = self.next_waiter.fetch_add(1, Ordering::Relaxed);
        let attached = {
            let mut inner = self.inner.lock().unwrap();
            if inner.session_grants.contains(&key) {
                return Ok(());
            }
            match inner.inflight.get(&key).cloned() {
                Some(id) => {
                    let (tx, rx) = oneshot::channel();
                    inner
                        .pending
                        .get_mut(&id)
                        .expect("inflight implies pending")
                        .waiters
                        .insert(wid, tx);
                    Some((id, rx))
                }
                None => None,
            }
        };
        if let Some((id, rx)) = attached {
            let guard = WaiterGuard {
                inner: &self.inner,
                id,
                wid,
            };
            let decision = rx.await.map_err(|_| PermissionError::Closed)?;
            drop(guard);
            return self.apply(decision, key, spec).await;
        }
        if self
            .store
            .has_grant(spec.tool.clone(), spec.action.clone(), spec.path.clone())
            .await?
        {
            return Ok(());
        }

        let (tx, rx) = oneshot::channel();
        let (request_id, request) = {
            let mut inner = self.inner.lock().unwrap();
            if inner.session_grants.contains(&key) {
                return Ok(());
            }
            match inner.inflight.get(&key).cloned() {
                Some(id) => {
                    inner
                        .pending
                        .get_mut(&id)
                        .expect("inflight implies pending")
                        .waiters
                        .insert(wid, tx);
                    (id, None)
                }
                None => {
                    let request = PermissionRequest {
                        id: RequestId::new(),
                        session: session.clone(),
                        tool: spec.tool.clone(),
                        action: spec.action.clone(),
                        path: spec.path.clone(),
                        description: spec.description.clone(),
                    };
                    inner.inflight.insert(key.clone(), request.id.clone());
                    inner.pending.insert(
                        request.id.clone(),
                        Pending {
                            key: key.clone(),
                            waiters: HashMap::from([(wid, tx)]),
                        },
                    );
                    (request.id.clone(), Some(request))
                }
            }
        };
        // Removes this waiter if the future is dropped or send fails, so
        // pending entries can't leak.
        let guard = WaiterGuard {
            inner: &self.inner,
            id: request_id,
            wid,
        };
        if let Some(request) = request {
            self.request_tx
                .send(request)
                .map_err(|_| PermissionError::Closed)?;
        }
        let decision = rx.await.map_err(|_| PermissionError::Closed)?;
        drop(guard);
        self.apply(decision, key, spec).await
    }

    async fn apply(
        &self,
        decision: Decision,
        key: SessionKey,
        spec: &PermissionSpec,
    ) -> Result<(), PermissionError> {
        match decision {
            Decision::Once => Ok(()),
            Decision::Session => {
                self.inner.lock().unwrap().session_grants.insert(key);
                Ok(())
            }
            Decision::Always => {
                self.store
                    .save_grant(spec.tool.clone(), spec.action.clone(), spec.path.clone())
                    .await?;
                Ok(())
            }
            Decision::Deny => Err(PermissionError::Denied(spec.description.clone())),
        }
    }

    /// Answer a pending request; every coalesced waiter gets the decision.
    pub fn resolve(&self, request: &RequestId, decision: Decision) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(pending) = inner.pending.remove(request) {
            inner.inflight.remove(&pending.key);
            for (_, tx) in pending.waiters {
                let _ = tx.send(decision);
            }
        }
    }
}

struct WaiterGuard<'a> {
    inner: &'a Mutex<Inner>,
    id: RequestId,
    wid: u64,
}

impl Drop for WaiterGuard<'_> {
    fn drop(&mut self) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(pending) = inner.pending.get_mut(&self.id) {
            pending.waiters.remove(&self.wid);
            if pending.waiters.is_empty() {
                let key = pending.key.clone();
                inner.pending.remove(&self.id);
                inner.inflight.remove(&key);
            }
        }
    }
}
