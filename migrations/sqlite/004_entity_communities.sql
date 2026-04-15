/** Adds community detection tables for two-tier (local + global) retrieval. */
CREATE TABLE entity_communities (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    summary         TEXT,
    project         TEXT,
    importance      REAL NOT NULL DEFAULT 0.0,
    embedding       BLOB CHECK (embedding IS NULL OR length(embedding) = 4096),
    created_at      TEXT NOT NULL
);

/** Maps entities to their detected communities (many-to-many). */
CREATE TABLE community_members (
    community_id    TEXT NOT NULL REFERENCES entity_communities(id) ON DELETE CASCADE,
    entity_id       TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    PRIMARY KEY (community_id, entity_id)
);

CREATE INDEX idx_ec_project ON entity_communities(project);
CREATE INDEX idx_cm_entity ON community_members(entity_id);
