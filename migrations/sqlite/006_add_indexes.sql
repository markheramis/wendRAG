-- PERF-09: partial index on active memory rows.
--
-- Every memory read and the maintenance cleanup job filter
-- `WHERE invalidated_at IS NULL`. Without this index, SQLite scans the
-- entire `memory_entries` table for each of those queries. A partial
-- index keeps the index small and covers the hot filter exactly.
CREATE INDEX IF NOT EXISTS idx_memory_entries_active
    ON memory_entries (created_at DESC)
    WHERE invalidated_at IS NULL;
