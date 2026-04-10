/** Adds PostgreSQL-native entity and relationship tables for graph retrieval. */
CREATE TABLE entities (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    normalized_name TEXT NOT NULL,
    name TEXT NOT NULL,
    entity_type VARCHAR(50) NOT NULL,
    description TEXT,
    embedding vector(1024),
    mention_count INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (normalized_name, entity_type)
);

/** Stores which entities are mentioned by each chunk. */
CREATE TABLE entity_mentions (
    chunk_id UUID NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
    entity_id UUID NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    PRIMARY KEY (chunk_id, entity_id)
);

/** Stores directed entity-to-entity relationships grounded in chunk evidence. */
CREATE TABLE entity_relationships (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    source_entity_id UUID NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    target_entity_id UUID NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    relationship_type VARCHAR(100) NOT NULL,
    description TEXT,
    weight DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    evidence_chunk_id UUID REFERENCES chunks(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_entities_embedding ON entities
    USING hnsw (embedding vector_cosine_ops);

CREATE INDEX idx_entities_type ON entities(entity_type);
CREATE INDEX idx_entity_mentions_chunk ON entity_mentions(chunk_id);
CREATE INDEX idx_entity_mentions_entity ON entity_mentions(entity_id);
CREATE INDEX idx_relationships_source ON entity_relationships(source_entity_id);
CREATE INDEX idx_relationships_target ON entity_relationships(target_entity_id);
CREATE INDEX idx_relationships_evidence_chunk ON entity_relationships(evidence_chunk_id);
