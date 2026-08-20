use std::path::PathBuf;
use std::sync::mpsc;

use rusqlite::Connection;
use rusqlite_migration::{M, Migrations};

use super::StoreError;

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// All SQLite access runs on one dedicated thread so the async runtime
/// never blocks on the database.
#[derive(Clone)]
pub(crate) struct Db {
    tx: mpsc::Sender<Job>,
}

impl Db {
    pub(crate) fn open(path: Option<PathBuf>) -> Result<Self, StoreError> {
        let mut conn = match &path {
            Some(path) => Connection::open(path)?,
            None => Connection::open_in_memory()?,
        };
        if path.is_some() {
            conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))?;
        }
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrations().to_latest(&mut conn)?;

        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("rushai-db".into())
            .spawn(move || {
                for job in rx {
                    job(&mut conn);
                }
            })?;
        Ok(Self { tx })
    }

    pub(crate) async fn call<T, F>(&self, f: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, StoreError> + Send + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(Box::new(move |conn| {
                let _ = tx.send(f(conn));
            }))
            .map_err(|_| StoreError::Closed)?;
        rx.await.map_err(|_| StoreError::Closed)?
    }
}

fn migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(include_str!("../../migrations/001_init.sql")),
        M::up(include_str!("../../migrations/002_permission_grants.sql")),
    ])
}

#[cfg(test)]
mod tests {
    use super::migrations;

    #[test]
    fn migrations_are_valid() {
        migrations().validate().unwrap();
    }
}
