/**
 * Domain types, error enums, and the entity extractor trait used across the
 * ingestion pipeline, storage backends, and retrieval layers.
 */

use async_trait::async_trait;

use crate::embed::provider::EmbeddingError;

/** Default graph traversal depth used when no explicit value is configured. */
pub const DEFAULT_GRAPH_TRAVERSAL_DEPTH: u8 = 2;

pub(crate) const MAX_GRAPH_TRAVERSAL_DEPTH: u8 = 3;

/** User-configurable graph retrieval settings shared by the server and tests. */
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphSettings {
    pub enabled: bool,
    pub traversal_depth: u8,
}

impl GraphSettings {
    /**
     * Normalizes the configured traversal depth into the supported recursion
     * window while preserving the caller's enabled/disabled toggle.
     */
    pub fn new(enabled: bool, traversal_depth: u8) -> Self {
        Self {
            enabled,
            traversal_depth: traversal_depth.clamp(1, MAX_GRAPH_TRAVERSAL_DEPTH),
        }
    }
}

/** Chunk-level input passed to an entity extractor implementation. */
#[derive(Debug, Clone, Copy)]
pub struct EntityExtractionInput<'a> {
    pub file_name: &'a str,
    pub file_type: &'a str,
    pub chunk_index: i32,
    pub section_title: Option<&'a str>,
    pub content: &'a str,
}

/** A single entity extracted from one chunk. */
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedEntity {
    pub name: String,
    pub entity_type: String,
    pub description: Option<String>,
}

/** A directed relationship extracted from one chunk. */
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedRelationship {
    pub source_name: String,
    pub source_type: String,
    pub target_name: String,
    pub target_type: String,
    pub relationship_type: String,
    pub description: Option<String>,
    pub weight: f32,
}

/** All extracted entities and relationships for a single chunk. */
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChunkEntityExtraction {
    pub chunk_index: i32,
    pub entities: Vec<ExtractedEntity>,
    pub relationships: Vec<ExtractedRelationship>,
}

/** Document-level graph payload persisted by the storage backend. */
#[derive(Debug, Clone, Default)]
pub struct DocumentEntityGraph {
    pub entities: Vec<EntityNode>,
    pub mentions: Vec<EntityMention>,
    pub relationships: Vec<EntityEdge>,
}

impl DocumentEntityGraph {
    /**
     * Reports whether the graph contains any entities, mentions, or
     * relationships that need to be persisted for the document.
     */
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty() && self.mentions.is_empty() && self.relationships.is_empty()
    }
}

/** Canonical entity row data persisted in the backend graph tables. */
#[derive(Debug, Clone)]
pub struct EntityNode {
    pub normalized_name: String,
    pub display_name: String,
    pub entity_type: String,
    pub description: Option<String>,
    pub embedding: Vec<f32>,
}

/** Chunk-to-entity join row persisted in the backend graph tables. */
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EntityMention {
    pub chunk_index: i32,
    pub normalized_name: String,
    pub entity_type: String,
}

/** Directed relationship row persisted in the backend graph tables. */
#[derive(Debug, Clone, PartialEq)]
pub struct EntityEdge {
    pub source_normalized_name: String,
    pub source_type: String,
    pub target_normalized_name: String,
    pub target_type: String,
    pub relationship_type: String,
    pub description: Option<String>,
    pub weight: f32,
    pub evidence_chunk_index: i32,
}

#[derive(Debug, thiserror::Error)]
pub enum EntityExtractionError {
    #[error("entity extraction request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("entity extraction API returned error: {status} - {body}")]
    Api { status: u16, body: String },
    #[error("entity extraction response did not contain text content")]
    MissingContent,
    #[error("entity extraction response contained invalid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum EntityGraphBuildError {
    #[error("entity embedding failed: {0}")]
    Embedding(#[from] EmbeddingError),
    #[error("entity embedding count mismatch: expected {expected}, received {actual}")]
    EmbeddingCountMismatch { expected: usize, actual: usize },
}

/** Contract for optional chunk-level entity extraction providers. */
#[async_trait]
pub trait EntityExtractor: Send + Sync {
    async fn extract(
        &self,
        input: EntityExtractionInput<'_>,
    ) -> Result<ChunkEntityExtraction, EntityExtractionError>;
}
