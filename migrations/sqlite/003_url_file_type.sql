PRAGMA foreign_keys = OFF;

DROP TRIGGER IF EXISTS documents_ai;
DROP TRIGGER IF EXISTS documents_ad;
DROP TRIGGER IF EXISTS documents_au;
DROP TABLE IF EXISTS document_paths_trigram;

CREATE TABLE documents_new (
    id TEXT PRIMARY KEY,
    file_path TEXT NOT NULL UNIQUE,
    file_name TEXT NOT NULL,
    file_type TEXT NOT NULL CHECK (file_type IN ('markdown', 'text', 'pdf', 'url')),
    content_hash TEXT NOT NULL,
    project TEXT,
    tags TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

INSERT INTO documents_new (id, file_path, file_name, file_type, content_hash, project, tags, created_at, updated_at)
SELECT id, file_path, file_name, file_type, content_hash, project, tags, created_at, updated_at
FROM documents;

DROP TABLE documents;

ALTER TABLE documents_new RENAME TO documents;

CREATE INDEX documents_project_idx ON documents(project);
CREATE INDEX documents_file_type_idx ON documents(file_type);

CREATE VIRTUAL TABLE document_paths_trigram USING fts5(
    file_path,
    content='documents',
    content_rowid='rowid',
    tokenize='trigram'
);

INSERT INTO document_paths_trigram(rowid, file_path)
SELECT rowid, file_path
FROM documents;

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

PRAGMA foreign_keys = ON;
