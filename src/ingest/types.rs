/**
 * Shared types, error enums, and configuration structs used across the
 * single-file, directory, and inline-content ingestion flows.
 */

use std::sync::Arc;

use serde::Serialize;

use crate::config::{ChunkingStrategy, CommunityConfig};
use crate::entity::{EntityExtractionError, EntityExtractor, EntityGraphBuildError};

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("unsupported file type: {0}")]
    UnsupportedType(String),
    #[error("invalid path: {0}")]
    InvalidPath(String),
    #[error("invalid glob pattern: {0}")]
    InvalidGlobPattern(String),
    #[error("file read error: {0}")]
    Read(#[from] super::reader::ReadError),
    #[error("embedding error: {0}")]
    Embedding(#[from] crate::embed::provider::EmbeddingError),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("chunking error: {0}")]
    Chunking(#[from] super::chunker::ChunkingError),
    #[error("entity extraction error: {0}")]
    EntityExtraction(#[from] EntityExtractionError),
    #[error("entity graph build error: {0}")]
    EntityGraphBuild(#[from] EntityGraphBuildError),
}

#[derive(Debug, Clone)]
pub enum IngestStatus {
    Created,
    Updated,
    Unchanged,
}

impl std::fmt::Display for IngestStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Created => write!(f, "created"),
            Self::Updated => write!(f, "updated"),
            Self::Unchanged => write!(f, "unchanged"),
        }
    }
}

#[derive(Debug)]
pub struct IngestResult {
    pub document_id: uuid::Uuid,
    pub file_path: String,
    pub chunk_count: usize,
    pub status: IngestStatus,
}

#[derive(Debug, Serialize)]
pub struct IngestDocumentStatus {
    pub file_path: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct IngestPathResult {
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub deleted: usize,
    pub failed: usize,
    pub documents: Vec<IngestDocumentStatus>,
}

#[derive(Clone)]
pub struct IngestOptions {
    pub project: Option<String>,
    pub tags: Vec<String>,
    pub entity_extractor: Option<Arc<dyn EntityExtractor>>,
    pub community_config: Option<CommunityConfig>,
    pub chunking_strategy: ChunkingStrategy,
    pub semantic_threshold: f64,
    pub max_sentences: usize,
    pub filter_garbage: bool,
    /// Enables SSRF protection for URL ingestion (blocks private/loopback IPs).
    /// Defaults to `true`; integration tests set this to `false` to reach
    /// local test servers.
    pub enforce_ssrf: bool,
}

impl IngestOptions {
    /**
     * Captures the shared ingest options passed through file, directory, and
     * inline-content ingestion flows.
     */
    pub fn new(
        project: Option<&str>,
        tags: &[String],
        entity_extractor: Option<&Arc<dyn EntityExtractor>>,
        community_config: Option<CommunityConfig>,
        chunking_strategy: ChunkingStrategy,
        semantic_threshold: f64,
        max_sentences: usize,
        filter_garbage: bool,
    ) -> Self {
        Self {
            project: project.map(ToOwned::to_owned),
            tags: tags.to_vec(),
            entity_extractor: entity_extractor.cloned(),
            community_config,
            chunking_strategy,
            semantic_threshold,
            max_sentences,
            filter_garbage,
            enforce_ssrf: true,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DirectoryIngestRequest<'a> {
    pub directory_path: &'a str,
    pub recursive: bool,
    pub glob_pattern: Option<&'a str>,
    pub delete_removed: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct ContentIngestRequest<'a> {
    pub file_path: &'a str,
    pub file_name: &'a str,
    pub file_type: &'a str,
    pub text: &'a str,
}
