use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use rushai_protocol::{Decision, PermissionRequest, RequestId, SessionId};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

use crate::store::{Store, StoreError};

/// What a tool wants to do; the grant key is {tool, action, path}.
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

type GrantKey = (String, String, String);

/// Checks grants and asks the frontend when none exist.
/// Requests go out on the channel returned by [`PermissionService::new`];
/// whoever owns the receiver answers via [`PermissionService::resolve`].
pub struct PermissionService {
    yolo: bool,
    store: Store,
    request_tx: mpsc::UnboundedSender<PermissionRequest>,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    session_grants: HashSet<(SessionId, GrantKey)>,
    pending: HashMap<RequestId, oneshot::Sender<Decision>>,
}

impl PermissionService {
    pub fn new(yolo: bool, store: Store) -> (Self, mpsc::UnboundedReceiver<PermissionRequest>) {
        let (request_tx, request_rx) = mpsc::unbounded_channel();
        (
            Self {
                yolo,
                store,
                request_tx,
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
        let key = grant_key(spec);
        if self
            .inner
            .lock()
            .unwrap()
            .session_grants
            .contains(&(session.clone(), key.clone()))
        {
            return Ok(());
        }
        if self
            .store
            .has_grant(spec.tool.clone(), spec.action.clone(), spec.path.clone())
            .await?
        {
            return Ok(());
        }

        let request = PermissionRequest {
            id: RequestId::new(),
            session: session.clone(),
            tool: spec.tool.clone(),
            action: spec.action.clone(),
            path: spec.path.clone(),
            description: spec.description.clone(),
        };
        let (tx, rx) = oneshot::channel();
        self.inner
            .lock()
            .unwrap()
            .pending
            .insert(request.id.clone(), tx);
        self.request_tx
            .send(request)
            .map_err(|_| PermissionError::Closed)?;

        match rx.await.map_err(|_| PermissionError::Closed)? {
            Decision::Once => Ok(()),
            Decision::Session => {
                self.inner
                    .lock()
                    .unwrap()
                    .session_grants
                    .insert((session.clone(), key));
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

    pub fn resolve(&self, request: &RequestId, decision: Decision) {
        if let Some(tx) = self.inner.lock().unwrap().pending.remove(request) {
            let _ = tx.send(decision);
        }
    }
}

fn grant_key(spec: &PermissionSpec) -> GrantKey {
    (
        spec.tool.clone(),
        spec.action.clone(),
        spec.path.clone().unwrap_or_default(),
    )
}
