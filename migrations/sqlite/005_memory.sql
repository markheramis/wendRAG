/** Adds persistent memory storage tables for the agent session/memory layer. */
CREATE TABLE memory_entries (
    id                  TEXT PRIMARY KEY,
    scope               TEXT NOT NULL,
    session_id          TEXT,
    user_id             TEXT,
    content             TEXT NOT NULL,
    entry_type          TEXT NOT NULL,
    importance_score    REAL NOT NULL DEFAULT 0.5,
    created_at          TEXT NOT NULL,
    last_accessed       TEXT NOT NULL,
    access_count        INTEGER NOT NULL DEFAULT 0,
    invalidated_at      TEXT,
    source              TEXT NOT NULL DEFAULT 'memory_system',
    ttl_seconds         INTEGER,
    metadata            TEXT DEFAULT '{}',
    embedding           BLOB CHECK (embedding IS NULL OR length(embedding) = 4096)
);

/** Maps memory entries to entities for graph-augmented recall. */
CREATE TABLE memory_entity_links (
    memory_id           TEXT NOT NULL REFERENCES memory_entries(id) ON DELETE CASCADE,
    entity_id           TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    relationship_type   TEXT,
    PRIMARY KEY (memory_id, entity_id)
);

CREATE INDEX idx_mem_session ON memory_entries(session_id, created_at DESC);
CREATE INDEX idx_mem_user ON memory_entries(user_id, importance_score DESC);
CREATE INDEX idx_mem_scope_type ON memory_entries(scope, entry_type);
CREATE INDEX idx_mem_accessed ON memory_entries(last_accessed DESC);
CREATE INDEX idx_mem_entity ON memory_entity_links(entity_id);
