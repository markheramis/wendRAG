-- Voyage AI models (e.g. voyage-3, voyage-3.5) return 1024-dimensional embeddings.
-- Align the column with the provider output; drop HNSW first because dimension change invalidates it.

DROP INDEX IF EXISTS chunks_embedding_idx;

ALTER TABLE chunks
    ALTER COLUMN embedding TYPE vector(1024);

CREATE INDEX chunks_embedding_idx ON chunks
    USING hnsw (embedding vector_cosine_ops);
