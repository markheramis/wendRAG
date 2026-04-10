/** Adds entity and relationship tables for graph-boosted retrieval. */
CREATE TABLE entities (
    id TEXT PRIMARY KEY,
    normalized_name TEXT NOT NULL,
    name TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    description TEXT,
    embedding BLOB CHECK (embedding IS NULL OR length(embedding) = 4096),
    mention_count INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    UNIQUE (normalized_name, entity_type)
);

/** Stores which entities are mentioned by each chunk. */
CREATE TABLE entity_mentions (
    chunk_id TEXT NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
    entity_id TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    PRIMARY KEY (chunk_id, entity_id)
);

/** Stores directed entity-to-entity relationships grounded in chunk evidence. */
CREATE TABLE entity_relationships (
    id TEXT PRIMARY KEY,
    source_entity_id TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    target_entity_id TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    relationship_type TEXT NOT NULL,
    description TEXT,
    weight REAL NOT NULL DEFAULT 1.0,
    evidence_chunk_id TEXT REFERENCES chunks(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX idx_entities_type ON entities(entity_type);
CREATE INDEX idx_entities_normalized_name ON entities(normalized_name);
CREATE INDEX idx_entity_mentions_chunk ON entity_mentions(chunk_id);
CREATE INDEX idx_entity_mentions_entity ON entity_mentions(entity_id);
CREATE INDEX idx_relationships_source ON entity_relationships(source_entity_id);
CREATE INDEX idx_relationships_target ON entity_relationships(target_entity_id);
CREATE INDEX idx_relationships_evidence_chunk ON entity_relationships(evidence_chunk_id);
