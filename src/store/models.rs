use chrono::{DateTime, Utc};
use sqlx::FromRow;
use uuid::Uuid;

/// Lightweight projection of the `documents` table used by the ingest
/// pipeline to check content freshness and detect creates vs. updates.
#[derive(Debug, Clone, FromRow)]
pub struct Document {
    pub id: Uuid,
    pub content_hash: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/**
 * Read-only projection of a detected entity community. Embeddings are kept
 * internal to the storage layer (used only for ANN search), so this struct
 * stays lean for serialization and MCP resource responses.
 */
#[derive(Debug, Clone)]
pub struct StoredCommunity {
    pub id: Uuid,
    pub name: String,
    pub summary: Option<String>,
    pub project: Option<String>,
    pub importance: f32,
    pub entity_count: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct DocumentWithChunkCount {
    pub id: Uuid,
    pub file_path: String,
    pub file_name: String,
    pub file_type: String,
    pub project: Option<String>,
    pub tags: Vec<String>,
    pub chunk_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Ordered chunk projection returned by
/// [`crate::store::StorageBackend::get_document_chunks`]. Used by the
/// `rag://documents/{id}` MCP resource to display the full chunk listing
/// for a document.
#[derive(Debug, Clone, FromRow)]
pub struct DocumentChunk {
    pub content: String,
    pub chunk_index: i32,
    pub section_title: Option<String>,
}

/// Chunk projection returned by
/// [`crate::store::StorageBackend::get_chunks_by_index`]. Carries the
/// document identifiers in addition to the chunk body so callers of the
/// `rag_get_chunk` tool can correlate the response with a `rag_get_context`
/// hit without a second round-trip.
#[derive(Debug, Clone, FromRow)]
pub struct DocumentChunkWithMeta {
    pub document_id: Uuid,
    pub file_path: String,
    pub file_name: String,
    pub chunk_index: i32,
    pub section_title: Option<String>,
    pub content: String,
}
