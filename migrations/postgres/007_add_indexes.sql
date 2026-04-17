-- PERF-09: add indexes that every query on memory_entries and entities
-- would otherwise force a sequential scan for.

-- Partial index on active memory entries. Every memory read filters
-- `WHERE invalidated_at IS NULL`, and the invalidation maintenance job
-- also drives lookups by this column. A partial index keeps the index
-- small (only live rows) and fast.
CREATE INDEX IF NOT EXISTS idx_memory_entries_active
    ON memory_entries (created_at DESC)
    WHERE invalidated_at IS NULL;

-- Entity normalisation lookups during ingest (to find an existing row
-- before falling through to the ON CONFLICT branch) and during retrieval
-- (community + graph joins) both hit `normalized_name`. The SQLite
-- backend already has an index on this column; Postgres only had the
-- composite UNIQUE (normalized_name, entity_type), which cannot serve
-- prefix-only lookups efficiently.
CREATE INDEX IF NOT EXISTS idx_entities_normalized_name
    ON entities (normalized_name);
