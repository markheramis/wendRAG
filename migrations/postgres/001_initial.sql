CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE TABLE documents (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    file_path TEXT NOT NULL UNIQUE,
    file_name TEXT NOT NULL,
    file_type TEXT NOT NULL CHECK (file_type IN ('markdown', 'text', 'pdf')),
    content_hash TEXT NOT NULL,
    project TEXT,
    tags TEXT[] NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE chunks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    document_id UUID NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    content TEXT NOT NULL,
    chunk_index INTEGER NOT NULL,
    section_title TEXT,

    embedding vector(1536),

    search_tsv tsvector GENERATED ALWAYS AS (
        setweight(to_tsvector('english', coalesce(section_title, '')), 'A') ||
        setweight(to_tsvector('english', content), 'B')
    ) STORED,

    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    UNIQUE (document_id, chunk_index)
);

CREATE INDEX chunks_embedding_idx ON chunks
    USING hnsw (embedding vector_cosine_ops);

CREATE INDEX chunks_search_tsv_idx ON chunks
    USING gin (search_tsv);

CREATE INDEX chunks_section_title_trgm_idx ON chunks
    USING gin (section_title gin_trgm_ops);

CREATE INDEX documents_file_path_trgm_idx ON documents
    USING gin (file_path gin_trgm_ops);

CREATE INDEX documents_project_idx ON documents(project);
CREATE INDEX documents_file_type_idx ON documents(file_type);
CREATE INDEX chunks_document_id_idx ON chunks(document_id);
