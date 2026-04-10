/** Allows URL-backed documents for HTML ingestion. */
ALTER TABLE documents
    DROP CONSTRAINT IF EXISTS documents_file_type_check;

ALTER TABLE documents
    ADD CONSTRAINT documents_file_type_check
    CHECK (file_type IN ('markdown', 'text', 'pdf', 'url'));
