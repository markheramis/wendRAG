/** Adds community detection tables for two-tier (local + global) retrieval. */
CREATE TABLE entity_communities (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name            TEXT NOT NULL,
    summary         TEXT,
    project         TEXT,
    importance      FLOAT NOT NULL DEFAULT 0.0,
    embedding       vector(1024),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

/** Maps entities to their detected communities (many-to-many). */
CREATE TABLE community_members (
    community_id    UUID NOT NULL REFERENCES entity_communities(id) ON DELETE CASCADE,
    entity_id       UUID NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    PRIMARY KEY (community_id, entity_id)
);

CREATE INDEX idx_ec_project ON entity_communities(project);
CREATE INDEX idx_ec_embedding ON entity_communities
    USING hnsw (embedding vector_cosine_ops);
CREATE INDEX idx_cm_entity ON community_members(entity_id);
