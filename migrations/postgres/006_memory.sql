/** Adds persistent memory storage tables for the agent session/memory layer. */
CREATE TABLE memory_entries (
    id                  UUID PRIMARY KEY,
    scope               VARCHAR(20) NOT NULL,
    session_id          TEXT,
    user_id             TEXT,
    content             TEXT NOT NULL,
    entry_type          VARCHAR(50) NOT NULL,
    importance_score    FLOAT NOT NULL DEFAULT 0.5,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_accessed       TIMESTAMPTZ NOT NULL DEFAULT now(),
    access_count        INTEGER NOT NULL DEFAULT 0,
    invalidated_at      TIMESTAMPTZ,
    source              VARCHAR(255) NOT NULL DEFAULT 'memory_system',
    ttl_seconds         INTEGER,
    metadata            JSONB DEFAULT '{}',
    embedding           vector(1024)
);

/** Maps memory entries to entities for graph-augmented recall. */
CREATE TABLE memory_entity_links (
    memory_id           UUID NOT NULL REFERENCES memory_entries(id) ON DELETE CASCADE,
    entity_id           UUID NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    relationship_type   VARCHAR(50),
    PRIMARY KEY (memory_id, entity_id)
);

CREATE INDEX idx_mem_session ON memory_entries(session_id, created_at DESC);
CREATE INDEX idx_mem_user ON memory_entries(user_id, importance_score DESC);
CREATE INDEX idx_mem_scope_type ON memory_entries(scope, entry_type);
CREATE INDEX idx_mem_accessed ON memory_entries(last_accessed DESC);
CREATE INDEX idx_mem_embedding ON memory_entries
    USING hnsw (embedding vector_cosine_ops);
CREATE INDEX idx_mem_entity ON memory_entity_links(entity_id);
