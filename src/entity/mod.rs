/**
 * Entity extraction, graph aggregation, and normalization for the wendRAG
 * knowledge graph pipeline.
 */

mod community;
mod community_manager;
mod extractor;
mod graph_build;
pub(crate) mod normalize;
mod model;

pub use community::{
    CommunityDetectionConfig, EntityCommunity, detect_communities,
};
pub use community_manager::{
    CommunityManager, CommunityManagerError, CommunityWithSummary,
    MIN_ENTITIES_FOR_COMMUNITIES, TwoTierRetrievalResult,
};
pub use extractor::OpenAiCompatEntityExtractor;
pub use graph_build::build_document_entity_graph;
pub use model::{
    ChunkEntityExtraction, DocumentEntityGraph, EntityEdge, EntityExtractionError,
    EntityExtractionInput, EntityExtractor, EntityGraphBuildError, EntityMention, EntityNode,
    ExtractedEntity, ExtractedRelationship, GraphSettings, DEFAULT_GRAPH_TRAVERSAL_DEPTH,
};
