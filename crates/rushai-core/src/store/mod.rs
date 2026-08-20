mod db;

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rushai_protocol::{MessageId, Part, Role, SessionId};
use rusqlite::{OptionalExtension, Row, params};
use thiserror::Error;

use db::Db;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("migration failed: {0}")]
    Migration(#[from] rusqlite_migration::Error),
    #[error("corrupt message parts: {0}")]
    Parts(#[from] serde_json::Error),
    #[error("invalid role {0:?} stored in database")]
    InvalidRole(String),
    #[error("store thread is gone")]
    Closed,
    #[error("failed to start store thread: {0}")]
    Thread(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub id: SessionId,
    pub parent: Option<SessionId>,
    pub title: String,
    pub summary_message_id: Option<MessageId>,
    pub cost: f64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredMessage {
    pub id: MessageId,
    pub session: SessionId,
    pub role: Role,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub parts: Vec<Part>,
    pub is_summary: bool,
    pub created_at: i64,
}

pub struct Store {
    db: Db,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Ok(Self {
            db: Db::open(Some(path.as_ref().to_path_buf()))?,
        })
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        Ok(Self {
            db: Db::open(None)?,
        })
    }

    pub async fn create_session(
        &self,
        title: String,
        parent: Option<SessionId>,
    ) -> Result<Session, StoreError> {
        let now = now_ms();
        let session = Session {
            id: SessionId::new(),
            parent,
            title,
            summary_message_id: None,
            cost: 0.0,
            prompt_tokens: 0,
            completion_tokens: 0,
            created_at: now,
            updated_at: now,
        };
        let row = session.clone();
        self.db
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO sessions (id, parent_session_id, title, cost, prompt_tokens, \
                     completion_tokens, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        row.id.as_str(),
                        row.parent.as_ref().map(SessionId::as_str),
                        row.title,
                        row.cost,
                        row.prompt_tokens as i64,
                        row.completion_tokens as i64,
                        row.created_at,
                        row.updated_at,
                    ],
                )?;
                Ok(())
            })
            .await?;
        Ok(session)
    }

    pub async fn session(&self, id: &SessionId) -> Result<Option<Session>, StoreError> {
        let id = id.clone();
        self.db
            .call(move |conn| {
                conn.query_row(
                    &format!("{SESSION_SELECT} WHERE id = ?1"),
                    params![id.as_str()],
                    session_from_row,
                )
                .optional()
                .map_err(Into::into)
            })
            .await
    }

    /// All sessions, most recently updated first.
    pub async fn sessions(&self) -> Result<Vec<Session>, StoreError> {
        self.db
            .call(move |conn| {
                let mut stmt = conn.prepare(&format!(
                    "{SESSION_SELECT} ORDER BY updated_at DESC, id DESC"
                ))?;
                let rows = stmt.query_map([], session_from_row)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
            })
            .await
    }

    pub async fn delete_session(&self, id: &SessionId) -> Result<(), StoreError> {
        let id = id.clone();
        self.db
            .call(move |conn| {
                conn.execute("DELETE FROM sessions WHERE id = ?1", params![id.as_str()])?;
                Ok(())
            })
            .await
    }

    /// Insert or update a message and touch the session's updated_at.
    pub async fn save_message(&self, message: &StoredMessage) -> Result<(), StoreError> {
        let parts = serde_json::to_string(&message.parts)?;
        let row = message.clone();
        self.db
            .call(move |conn| {
                let tx = conn.transaction()?;
                tx.execute(
                    "INSERT INTO messages (id, session_id, role, provider, model, parts, \
                     is_summary, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
                     ON CONFLICT (id) DO UPDATE SET parts = excluded.parts, \
                     is_summary = excluded.is_summary",
                    params![
                        row.id.as_str(),
                        row.session.as_str(),
                        role_str(row.role),
                        row.provider,
                        row.model,
                        parts,
                        row.is_summary,
                        row.created_at,
                    ],
                )?;
                tx.execute(
                    "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
                    params![now_ms(), row.session.as_str()],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
    }

    /// Messages for a session in creation order.
    pub async fn messages(&self, session: &SessionId) -> Result<Vec<StoredMessage>, StoreError> {
        let session = session.clone();
        self.db
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT id, session_id, role, provider, model, parts, is_summary, created_at \
                     FROM messages WHERE session_id = ?1 ORDER BY created_at, id",
                )?;
                let rows = stmt.query_map(params![session.as_str()], message_from_row)?;
                rows.collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .map(RawMessage::parse)
                    .collect()
            })
            .await
    }
}

const SESSION_SELECT: &str = "SELECT id, parent_session_id, title, summary_message_id, cost, \
                              prompt_tokens, completion_tokens, created_at, updated_at \
                              FROM sessions";

fn session_from_row(row: &Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: SessionId::from(row.get::<_, String>(0)?),
        parent: row.get::<_, Option<String>>(1)?.map(SessionId::from),
        title: row.get(2)?,
        summary_message_id: row.get::<_, Option<String>>(3)?.map(MessageId::from),
        cost: row.get(4)?,
        prompt_tokens: row.get::<_, i64>(5)? as u64,
        completion_tokens: row.get::<_, i64>(6)? as u64,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

struct RawMessage {
    id: String,
    session: String,
    role: String,
    provider: Option<String>,
    model: Option<String>,
    parts: String,
    is_summary: bool,
    created_at: i64,
}

fn message_from_row(row: &Row<'_>) -> rusqlite::Result<RawMessage> {
    Ok(RawMessage {
        id: row.get(0)?,
        session: row.get(1)?,
        role: row.get(2)?,
        provider: row.get(3)?,
        model: row.get(4)?,
        parts: row.get(5)?,
        is_summary: row.get(6)?,
        created_at: row.get(7)?,
    })
}

impl RawMessage {
    fn parse(self) -> Result<StoredMessage, StoreError> {
        let role = match self.role.as_str() {
            "user" => Role::User,
            "assistant" => Role::Assistant,
            other => return Err(StoreError::InvalidRole(other.to_owned())),
        };
        Ok(StoredMessage {
            id: MessageId::from(self.id),
            session: SessionId::from(self.session),
            role,
            provider: self.provider,
            model: self.model,
            parts: serde_json::from_str(&self.parts)?,
            is_summary: self.is_summary,
            created_at: self.created_at,
        })
    }
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as i64
}
