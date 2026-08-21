//! The agent loop.
//!
//! One actor task owns all session state and consumes [`Op`]s; runs execute
//! as child tasks that report back through an internal channel. Ownership
//! replaces locking: only the actor touches accept sequences, queues, and
//! cancel marks, so accept-versus-cancel races cannot happen by construction.

mod run;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use futures::FutureExt;
use rushai_protocol::{Event, FinishReason, Op, SessionId, UserPart};
use rushai_provider::Provider;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::permission::PermissionService;
use crate::store::Store;
use crate::tool::Tool;

pub struct EngineConfig {
    pub store: Store,
    pub provider: Arc<dyn Provider>,
    pub tools: Vec<Arc<dyn Tool>>,
    pub cwd: PathBuf,
    pub yolo: bool,
    pub max_tokens: Option<u64>,
}

/// Handle to the engine actor.
#[derive(Clone)]
pub struct Engine {
    op_tx: mpsc::UnboundedSender<Op>,
}

impl Engine {
    pub fn spawn(config: EngineConfig) -> (Self, mpsc::Receiver<Event>) {
        let (op_tx, op_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::channel(256);
        let (done_tx, done_rx) = mpsc::unbounded_channel();
        let (permissions, request_rx) = PermissionService::new(config.yolo, config.store.clone());
        let actor = Actor {
            store: config.store,
            provider: config.provider,
            tools: config.tools.into(),
            permissions: Arc::new(permissions),
            cwd: config.cwd,
            system: run::system_prompt(),
            max_tokens: config.max_tokens,
            events: event_tx,
            done_tx,
            next_seq: 0,
            sessions: HashMap::new(),
        };
        tokio::spawn(actor.run(op_rx, done_rx, request_rx));
        (Self { op_tx }, event_rx)
    }

    /// False when the engine is gone.
    pub fn submit(&self, op: Op) -> bool {
        self.op_tx.send(op).is_ok()
    }
}

struct Actor {
    store: Store,
    provider: Arc<dyn Provider>,
    tools: Arc<[Arc<dyn Tool>]>,
    permissions: Arc<PermissionService>,
    cwd: PathBuf,
    system: String,
    max_tokens: Option<u64>,
    events: mpsc::Sender<Event>,
    done_tx: mpsc::UnboundedSender<RunDone>,
    next_seq: u64,
    sessions: HashMap<SessionId, SessionState>,
}

#[derive(Default)]
struct SessionState {
    active: Option<Active>,
    queue: VecDeque<Queued>,
    /// Runs accepted at or before this sequence must not start.
    cancel_mark: u64,
    last_seq: u64,
}

struct Active {
    seq: u64,
    cancel: CancellationToken,
}

struct Queued {
    seq: u64,
    parts: Vec<UserPart>,
}

struct RunDone {
    session: SessionId,
    seq: u64,
    reason: FinishReason,
}

impl Actor {
    async fn run(
        mut self,
        mut op_rx: mpsc::UnboundedReceiver<Op>,
        mut done_rx: mpsc::UnboundedReceiver<RunDone>,
        mut request_rx: mpsc::UnboundedReceiver<rushai_protocol::PermissionRequest>,
    ) {
        loop {
            tokio::select! {
                op = op_rx.recv() => match op {
                    Some(op) => {
                        if self.handle_op(op).await {
                            break;
                        }
                    }
                    None => break,
                },
                Some(done) = done_rx.recv() => self.handle_done(done).await,
                Some(request) = request_rx.recv() => {
                    let _ = self.events.send(Event::PermissionRequested { request }).await;
                }
            }
        }
        for state in self.sessions.values() {
            if let Some(active) = &state.active {
                active.cancel.cancel();
            }
        }
    }

    /// True means shutdown.
    async fn handle_op(&mut self, op: Op) -> bool {
        match op {
            Op::Prompt { session, parts } => {
                self.next_seq += 1;
                let seq = self.next_seq;
                let state = self.sessions.entry(session.clone()).or_default();
                state.last_seq = seq;
                let idle = state.active.is_none();
                let _ = self
                    .events
                    .send(Event::RunAccepted {
                        session: session.clone(),
                        seq,
                    })
                    .await;
                if idle {
                    self.start_run(session, seq, parts);
                } else {
                    let state = self.sessions.get_mut(&session).expect("state exists");
                    state.queue.push_back(Queued { seq, parts });
                }
            }
            Op::Cancel { session } => {
                if let Some(state) = self.sessions.get_mut(&session) {
                    state.cancel_mark = state.last_seq;
                    let dropped: Vec<u64> = state.queue.drain(..).map(|q| q.seq).collect();
                    if let Some(active) = &state.active {
                        active.cancel.cancel();
                    }
                    for seq in dropped {
                        let _ = self
                            .events
                            .send(Event::RunFinished {
                                session: session.clone(),
                                seq,
                                reason: FinishReason::Canceled,
                            })
                            .await;
                    }
                }
            }
            Op::PermissionDecision { request, decision } => {
                self.permissions.resolve(&request, decision);
            }
            Op::Compact { .. } => {
                // Summarization lands with the compaction PR.
            }
            Op::Shutdown => return true,
        }
        false
    }

    async fn handle_done(&mut self, done: RunDone) {
        let Some(state) = self.sessions.get_mut(&done.session) else {
            return;
        };
        if state.active.as_ref().is_some_and(|a| a.seq == done.seq) {
            state.active = None;
        }
        let _ = self
            .events
            .send(Event::RunFinished {
                session: done.session.clone(),
                seq: done.seq,
                reason: done.reason,
            })
            .await;
        // Belt and suspenders: Cancel drains the queue itself, but a prompt
        // that raced the drain still must not start.
        loop {
            let state = self.sessions.get_mut(&done.session).expect("state exists");
            match state.queue.pop_front() {
                Some(next) if next.seq <= state.cancel_mark => {
                    let _ = self
                        .events
                        .send(Event::RunFinished {
                            session: done.session.clone(),
                            seq: next.seq,
                            reason: FinishReason::Canceled,
                        })
                        .await;
                }
                Some(next) => {
                    self.start_run(done.session.clone(), next.seq, next.parts);
                    break;
                }
                None => break,
            }
        }
    }

    fn start_run(&mut self, session: SessionId, seq: u64, parts: Vec<UserPart>) {
        let cancel = CancellationToken::new();
        let state = self.sessions.entry(session.clone()).or_default();
        state.active = Some(Active {
            seq,
            cancel: cancel.clone(),
        });
        let ctx = run::RunCtx {
            store: self.store.clone(),
            provider: self.provider.clone(),
            tools: self.tools.clone(),
            permissions: self.permissions.clone(),
            events: self.events.clone(),
            cancel,
            session: session.clone(),
            cwd: self.cwd.clone(),
            system: self.system.clone(),
            max_tokens: self.max_tokens,
            prompt: parts,
        };
        let done_tx = self.done_tx.clone();
        tokio::spawn(async move {
            // A panicking run must still report, or its session wedges.
            let reason = std::panic::AssertUnwindSafe(run::run(ctx))
                .catch_unwind()
                .await
                .unwrap_or(FinishReason::Error);
            let _ = done_tx.send(RunDone {
                session,
                seq,
                reason,
            });
        });
    }
}
