CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    parent_session_id TEXT REFERENCES sessions (id),
    title TEXT NOT NULL DEFAULT '',
    summary_message_id TEXT,
    todos TEXT,
    cost REAL NOT NULL DEFAULT 0,
    prompt_tokens INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE messages (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    provider TEXT,
    model TEXT,
    parts TEXT NOT NULL,
    is_summary INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);

CREATE INDEX idx_messages_session_created ON messages (session_id, created_at);

CREATE TABLE files (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    version INTEGER NOT NULL,
    content TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE UNIQUE INDEX idx_files_session_path_version ON files (session_id, path, version);

CREATE TABLE read_files (
    session_id TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    mtime INTEGER NOT NULL,
    PRIMARY KEY (session_id, path)
);
