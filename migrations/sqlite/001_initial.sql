PRAGMA foreign_keys = ON;

CREATE TABLE documents (
    id TEXT PRIMARY KEY,
    file_path TEXT NOT NULL UNIQUE,
    file_name TEXT NOT NULL,
    file_type TEXT NOT NULL CHECK (file_type IN ('markdown', 'text', 'pdf')),
    content_hash TEXT NOT NULL,
    project TEXT,
    tags TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE chunks (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    content TEXT NOT NULL,
    chunk_index INTEGER NOT NULL,
    section_title TEXT,
    embedding BLOB NOT NULL CHECK (length(embedding) = 4096),
    created_at TEXT NOT NULL,
    UNIQUE (document_id, chunk_index)
);

CREATE INDEX documents_project_idx ON documents(project);
CREATE INDEX documents_file_type_idx ON documents(file_type);
CREATE INDEX chunks_document_id_idx ON chunks(document_id);

CREATE VIRTUAL TABLE chunks_fts USING fts5(
    section_title,
    content,
    content='chunks',
    content_rowid='rowid',
    tokenize='porter unicode61'
);

CREATE VIRTUAL TABLE chunk_titles_trigram USING fts5(
    section_title,
    content='chunks',
    content_rowid='rowid',
    tokenize='trigram'
);

CREATE VIRTUAL TABLE document_paths_trigram USING fts5(
    file_path,
    content='documents',
    content_rowid='rowid',
    tokenize='trigram'
);

CREATE TRIGGER chunks_ai AFTER INSERT ON chunks BEGIN
    INSERT INTO chunks_fts(rowid, section_title, content)
    VALUES (new.rowid, coalesce(new.section_title, ''), new.content);
    INSERT INTO chunk_titles_trigram(rowid, section_title)
    VALUES (new.rowid, coalesce(new.section_title, ''));
END;

CREATE TRIGGER chunks_ad AFTER DELETE ON chunks BEGIN
    INSERT INTO chunks_fts(chunks_fts, rowid, section_title, content)
    VALUES ('delete', old.rowid, coalesce(old.section_title, ''), old.content);
    INSERT INTO chunk_titles_trigram(chunk_titles_trigram, rowid, section_title)
    VALUES ('delete', old.rowid, coalesce(old.section_title, ''));
END;

CREATE TRIGGER chunks_au AFTER UPDATE ON chunks BEGIN
    INSERT INTO chunks_fts(chunks_fts, rowid, section_title, content)
    VALUES ('delete', old.rowid, coalesce(old.section_title, ''), old.content);
    INSERT INTO chunks_fts(rowid, section_title, content)
    VALUES (new.rowid, coalesce(new.section_title, ''), new.content);
    INSERT INTO chunk_titles_trigram(chunk_titles_trigram, rowid, section_title)
    VALUES ('delete', old.rowid, coalesce(old.section_title, ''));
    INSERT INTO chunk_titles_trigram(rowid, section_title)
    VALUES (new.rowid, coalesce(new.section_title, ''));
END;

CREATE TRIGGER documents_ai AFTER INSERT ON documents BEGIN
    INSERT INTO document_paths_trigram(rowid, file_path)
    VALUES (new.rowid, new.file_path);
END;

CREATE TRIGGER documents_ad AFTER DELETE ON documents BEGIN
    INSERT INTO document_paths_trigram(document_paths_trigram, rowid, file_path)
    VALUES ('delete', old.rowid, old.file_path);
END;

CREATE TRIGGER documents_au AFTER UPDATE ON documents BEGIN
    INSERT INTO document_paths_trigram(document_paths_trigram, rowid, file_path)
    VALUES ('delete', old.rowid, old.file_path);
    INSERT INTO document_paths_trigram(rowid, file_path)
    VALUES (new.rowid, new.file_path);
END;
