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

/// Ordered chunk projection used to reconstruct a readable document body from
/// stored chunk rows.
#[derive(Debug, Clone, FromRow)]
pub struct DocumentChunk {
    pub content: String,
    pub chunk_index: i32,
    pub section_title: Option<String>,
}
