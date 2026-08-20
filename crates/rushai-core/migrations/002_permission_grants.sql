CREATE TABLE permission_grants (
    tool TEXT NOT NULL,
    action TEXT NOT NULL,
    path TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    PRIMARY KEY (tool, action, path)
);
