/*!
 * Aggregates per-chunk entity extractions into a deduplicated document-level
 * graph and embeds canonical entity descriptions.
 */

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::embed::EmbeddingProvider;

use super::model::{
    ChunkEntityExtraction, DocumentEntityGraph, EntityEdge, EntityGraphBuildError, EntityMention,
    EntityNode,
};
use super::normalize::{
    clean_optional_text, clean_required_text, normalize_entity_name, normalize_entity_type,
    normalize_relationship_type,
};

#[derive(Debug, Clone)]
struct AggregatedEntity {
    normalized_name: String,
    display_name: String,
    entity_type: String,
    description: Option<String>,
}

/**
 * Builds a canonical document graph from per-chunk extraction output and embeds
 * the deduplicated entity descriptions with the existing embedding provider.
 */
pub async fn build_document_entity_graph(
    extractions: &[ChunkEntityExtraction],
    embedder: &Arc<dyn EmbeddingProvider>,
) -> Result<DocumentEntityGraph, EntityGraphBuildError> {
    let mut entities_by_key: BTreeMap<(String, String), AggregatedEntity> = BTreeMap::new();
    let mut mentions: BTreeSet<EntityMention> = BTreeSet::new();
    let mut relationships: Vec<EntityEdge> = Vec::new();

    for extraction in extractions {
        /*
         * Each chunk can mention the same entity multiple times; the join table
         * only needs one row per chunk/entity pair.
         */
        let mut mentioned_keys: BTreeSet<(String, String)> = BTreeSet::new();

        for entity in &extraction.entities {
            if let Some((normalized_name, entity_type)) = upsert_aggregated_entity(
                &mut entities_by_key,
                entity.name.as_str(),
                entity.entity_type.as_str(),
                entity.description.as_deref(),
            ) {
                mentioned_keys.insert((normalized_name, entity_type));
            }
        }

        for relationship in &extraction.relationships {
            let Some((source_name, source_type)) = upsert_aggregated_entity(
                &mut entities_by_key,
                relationship.source_name.as_str(),
                relationship.source_type.as_str(),
                None,
            ) else {
                continue;
            };
            let Some((target_name, target_type)) = upsert_aggregated_entity(
                &mut entities_by_key,
                relationship.target_name.as_str(),
                relationship.target_type.as_str(),
                None,
            ) else {
                continue;
            };

            if source_name == target_name && source_type == target_type {
                continue;
            }

            mentioned_keys.insert((source_name.clone(), source_type.clone()));
            mentioned_keys.insert((target_name.clone(), target_type.clone()));
            relationships.push(EntityEdge {
                source_normalized_name: source_name,
                source_type,
                target_normalized_name: target_name,
                target_type,
                relationship_type: normalize_relationship_type(
                    relationship.relationship_type.as_str(),
                ),
                description: clean_optional_text(relationship.description.as_deref()),
                weight: relationship.weight.max(0.1),
                evidence_chunk_index: extraction.chunk_index,
            });
        }

        for (normalized_name, entity_type) in mentioned_keys {
            mentions.insert(EntityMention {
                chunk_index: extraction.chunk_index,
                normalized_name,
                entity_type,
            });
        }
    }

    if entities_by_key.is_empty() {
        return Ok(DocumentEntityGraph::default());
    }

    let entity_texts: Vec<String> = entities_by_key
        .values()
        .map(render_entity_embedding_text)
        .collect();
    let embeddings = embedder.embed(&entity_texts).await?;
    if embeddings.len() != entities_by_key.len() {
        return Err(EntityGraphBuildError::EmbeddingCountMismatch {
            expected: entities_by_key.len(),
            actual: embeddings.len(),
        });
    }

    let entities = entities_by_key
        .into_values()
        .zip(embeddings)
        .map(|(entity, embedding)| EntityNode {
            normalized_name: entity.normalized_name,
            display_name: entity.display_name,
            entity_type: entity.entity_type,
            description: entity.description,
            embedding,
        })
        .collect();

    Ok(DocumentEntityGraph {
        entities,
        mentions: mentions.into_iter().collect(),
        relationships,
    })
}

/**
 * Inserts or updates the canonical entity entry used during document graph
 * aggregation and returns its normalized key.
 */
fn upsert_aggregated_entity(
    entities_by_key: &mut BTreeMap<(String, String), AggregatedEntity>,
    display_name: &str,
    entity_type: &str,
    description: Option<&str>,
) -> Option<(String, String)> {
    let normalized_name = normalize_entity_name(display_name);
    let normalized_type = normalize_entity_type(entity_type);
    let cleaned_description = clean_optional_text(description);

    if normalized_name.is_empty() {
        return None;
    }

    let key = (normalized_name.clone(), normalized_type.clone());
    let display_name = clean_required_text(display_name.to_string())
        .unwrap_or_else(|| display_name.trim().to_string());

    entities_by_key
        .entry(key.clone())
        .and_modify(|entity| {
            if entity.display_name.len() < display_name.len() {
                entity.display_name = display_name.clone();
            }
            if should_replace_description(
                entity.description.as_deref(),
                cleaned_description.as_deref(),
            ) {
                entity.description = cleaned_description.clone();
            }
        })
        .or_insert_with(|| AggregatedEntity {
            normalized_name: normalized_name.clone(),
            display_name,
            entity_type: normalized_type.clone(),
            description: cleaned_description,
        });

    Some(key)
}

/**
 * Decides whether a new description is more useful than the one already stored
 * for a canonical entity during aggregation.
 */
fn should_replace_description(current: Option<&str>, candidate: Option<&str>) -> bool {
    match (current, candidate) {
        (None, Some(_)) => true,
        (Some(current), Some(candidate)) => candidate.len() > current.len(),
        _ => false,
    }
}

/**
 * Renders the text embedded for one deduplicated entity so the stored vector
 * captures both the display name and any extracted description.
 */
fn render_entity_embedding_text(entity: &AggregatedEntity) -> String {
    match entity.description.as_deref() {
        Some(description) => format!(
            "{} [{}]\n{}",
            entity.display_name, entity.entity_type, description
        ),
        None => format!("{} [{}]", entity.display_name, entity.entity_type),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;

    use super::build_document_entity_graph;
    use crate::embed::EmbeddingProvider;
    use crate::embed::provider::EmbeddingError;
    use crate::entity::{
        ChunkEntityExtraction, EntityExtractionError, EntityExtractionInput, EntityExtractor,
        ExtractedEntity, ExtractedRelationship,
    };

    struct FakeEmbedder;

    #[async_trait]
    impl EmbeddingProvider for FakeEmbedder {
        /**
         * Produces deterministic embeddings so graph aggregation can be tested
         * without calling a live embeddings API.
         */
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
            Ok(texts
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    let mut embedding = vec![0.0; 1024];
                    embedding[index] = 1.0;
                    embedding
                })
                .collect())
        }
    }

    struct FakeExtractor;

    #[async_trait]
    impl EntityExtractor for FakeExtractor {
        /**
         * Returns a deterministic extraction used to verify the trait contract.
         */
        async fn extract(
            &self,
            input: EntityExtractionInput<'_>,
        ) -> Result<ChunkEntityExtraction, EntityExtractionError> {
            Ok(ChunkEntityExtraction {
                chunk_index: input.chunk_index,
                entities: vec![ExtractedEntity {
                    name: "Atlas".to_string(),
                    entity_type: "service".to_string(),
                    description: Some("Retrieval coordinator".to_string()),
                }],
                relationships: vec![ExtractedRelationship {
                    source_name: "Atlas".to_string(),
                    source_type: "service".to_string(),
                    target_name: "Beacon".to_string(),
                    target_type: "service".to_string(),
                    relationship_type: "depends on".to_string(),
                    description: Some("Atlas depends on Beacon".to_string()),
                    weight: 1.0,
                }],
            })
        }
    }

    /**
     * Verifies that chunk extraction output is deduplicated into canonical
     * document-level entities, mentions, and relationships.
     */
    #[tokio::test]
    async fn document_graph_builds_deduplicated_entities() {
        let extractor = FakeExtractor;
        let extraction = extractor
            .extract(EntityExtractionInput {
                file_name: "atlas.md",
                file_type: "markdown",
                chunk_index: 0,
                section_title: Some("Atlas"),
                content: "Atlas depends on Beacon.",
            })
            .await
            .unwrap();

        let embedder: Arc<dyn EmbeddingProvider> = Arc::new(FakeEmbedder);
        let graph = build_document_entity_graph(&[extraction], &embedder)
            .await
            .unwrap();

        assert_eq!(graph.entities.len(), 2);
        assert_eq!(graph.mentions.len(), 2);
        assert_eq!(graph.relationships.len(), 1);
        assert!(
            graph
                .entities
                .iter()
                .any(|entity| entity.normalized_name == "atlas" && entity.entity_type == "SERVICE")
        );
    }
}
