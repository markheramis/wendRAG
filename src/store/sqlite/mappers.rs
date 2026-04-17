/*!
 * Row-to-model mapping functions and codec helpers for the SQLite backend.
 */

use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;
use uuid::Uuid;

use crate::retrieve::ScoredChunk;
use crate::store::models::{
    Document, DocumentChunk, DocumentChunkWithMeta, DocumentWithChunkCount,
};

/**
 * Converts a JSON string column into the tag list used by the public models.
 */
pub(crate) fn decode_tags(value: String) -> Result<Vec<String>, sqlx::Error> {
    serde_json::from_str(&value).map_err(|error| sqlx::Error::Decode(Box::new(error)))
}

/**
 * Serializes tags as a JSON text array for the SQLite backend.
 */
pub(crate) fn encode_tags(tags: &[String]) -> Result<String, sqlx::Error> {
    serde_json::to_string(tags).map_err(|error| sqlx::Error::Encode(Box::new(error)))
}

/**
 * Parses a UUID stored as SQLite text.
 */
pub(crate) fn parse_uuid_text(value: String) -> Result<Uuid, sqlx::Error> {
    Uuid::parse_str(&value).map_err(|error| sqlx::Error::Decode(Box::new(error)))
}

/**
 * Parses a UTC timestamp stored as RFC3339 text in SQLite.
 */
pub(crate) fn parse_utc_timestamp(value: String) -> Result<DateTime<Utc>, sqlx::Error> {
    DateTime::parse_from_rfc3339(&value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|error| sqlx::Error::Decode(Box::new(error)))
}

/**
 * Maps the document freshness projection from a SQLite row into the shared
 * ingest model.
 */
pub(crate) fn map_document_row(row: SqliteRow) -> Result<Document, sqlx::Error> {
    Ok(Document {
        id: parse_uuid_text(row.try_get("id")?)?,
        content_hash: row.try_get("content_hash")?,
        created_at: parse_utc_timestamp(row.try_get("created_at")?)?,
        updated_at: parse_utc_timestamp(row.try_get("updated_at")?)?,
    })
}

/**
 * Maps the document listing projection from a SQLite row into the shared model.
 */
pub(crate) fn map_document_with_chunk_count_row(
    row: SqliteRow,
) -> Result<DocumentWithChunkCount, sqlx::Error> {
    Ok(DocumentWithChunkCount {
        id: parse_uuid_text(row.try_get("id")?)?,
        file_path: row.try_get("file_path")?,
        file_name: row.try_get("file_name")?,
        file_type: row.try_get("file_type")?,
        project: row.try_get("project")?,
        tags: decode_tags(row.try_get("tags")?)?,
        chunk_count: row.try_get("chunk_count")?,
        created_at: parse_utc_timestamp(row.try_get("created_at")?)?,
        updated_at: parse_utc_timestamp(row.try_get("updated_at")?)?,
    })
}

/**
 * Maps an ordered chunk projection from SQLite row data into the shared
 * `DocumentChunk` model. Used by `get_document_chunks`, which powers the
 * `rag://documents/{id}` MCP resource view.
 */
pub(crate) fn map_document_chunk_row(row: SqliteRow) -> Result<DocumentChunk, sqlx::Error> {
    Ok(DocumentChunk {
        content: row.try_get("content")?,
        chunk_index: row.try_get("chunk_index")?,
        section_title: row.try_get("section_title")?,
    })
}

/**
 * Maps the chunk-with-metadata projection used by
 * [`crate::store::StorageBackend::get_chunks_by_index`]. SQLite stores both
 * `documents.id` and `chunks.id` as TEXT, so the document UUID is parsed
 * back into a `Uuid` here rather than by `sqlx::FromRow`.
 */
pub(crate) fn map_document_chunk_with_meta_row(
    row: SqliteRow,
) -> Result<DocumentChunkWithMeta, sqlx::Error> {
    Ok(DocumentChunkWithMeta {
        document_id: parse_uuid_text(row.try_get("document_id")?)?,
        file_path: row.try_get("file_path")?,
        file_name: row.try_get("file_name")?,
        chunk_index: row.try_get("chunk_index")?,
        section_title: row.try_get("section_title")?,
        content: row.try_get("content")?,
    })
}

/**
 * Maps a shared scored-chunk projection from SQLite row data and an already
 * computed score.
 */
pub(crate) fn map_scored_chunk_row(row: SqliteRow, score: f64) -> Result<ScoredChunk, sqlx::Error> {
    Ok(ScoredChunk {
        chunk_id: parse_uuid_text(row.try_get("chunk_id")?)?,
        content: row.try_get("content")?,
        section_title: row.try_get("section_title")?,
        file_path: row.try_get("file_path")?,
        file_name: row.try_get("file_name")?,
        chunk_index: row.try_get("chunk_index")?,
        score,
    })
}
