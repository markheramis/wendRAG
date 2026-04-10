/**
 * Entity extraction, graph aggregation, and normalization for the wendRAG
 * knowledge graph pipeline.
 */

mod extractor;
mod graph_build;
pub(crate) mod normalize;
mod model;

pub use extractor::OpenAiCompatEntityExtractor;
pub use graph_build::build_document_entity_graph;
pub use model::{
    ChunkEntityExtraction, DocumentEntityGraph, EntityEdge, EntityExtractionError,
    EntityExtractionInput, EntityExtractor, EntityGraphBuildError, EntityMention, EntityNode,
    ExtractedEntity, ExtractedRelationship, GraphSettings, DEFAULT_GRAPH_TRAVERSAL_DEPTH,
};
