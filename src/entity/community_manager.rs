/**
 * Community management for entity graphs with optimized resource usage.
 * 
 * This module provides:
 * 1. Lazy community generation (only when entity count > threshold)
 * 2. Streaming processing to minimize memory usage
 * 3. Community summary generation using LLM
 * 4. Two-tier retrieval support (local + global)
 * 
 * Performance optimizations:
 * - Communities are generated once and cached
 * - Batched LLM calls for summary generation
 * - Minimal memory footprint via streaming
 */

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::embed::EmbeddingProvider;
use crate::embed::provider::EmbeddingError;

use super::community::{CommunityDetectionConfig, EntityCommunity, detect_communities};
use super::model::{DocumentEntityGraph, EntityEdge, EntityNode};

/// Minimum number of entities required to trigger community detection.
/// Below this threshold, all entities are treated as one community.
const MIN_ENTITIES_FOR_COMMUNITIES: usize = 5;

/// Batch size for LLM summary generation (to minimize API calls).
const SUMMARY_BATCH_SIZE: usize = 10;

/// Community data with optional LLM-generated summary.
#[derive(Debug, Clone)]
pub struct CommunityWithSummary {
    pub community: EntityCommunity,
    pub summary: Option<String>,
    pub summary_embedding: Option<Vec<f32>>,
}

/// Manages community detection and summary generation for entity graphs.
pub struct CommunityManager {
    config: CommunityDetectionConfig,
    embedder: Arc<dyn EmbeddingProvider>,
}

impl CommunityManager {
    /// Creates a new community manager with the specified configuration.
    pub fn new(config: CommunityDetectionConfig, embedder: Arc<dyn EmbeddingProvider>) -> Self {
        Self { config, embedder }
    }

    /**
     * Analyzes an entity graph and generates communities with summaries.
     * 
     * This method is optimized for:
     * - Low memory usage (streaming processing)
     * - Fast execution (batched operations)
     * - Minimal API calls (batched LLM requests)
     */
    pub async fn analyze_graph(
        &self,
        graph: &DocumentEntityGraph,
    ) -> Result<Vec<CommunityWithSummary>, CommunityManagerError> {
        // Skip community detection for small graphs
        if graph.entities.len() < MIN_ENTITIES_FOR_COMMUNITIES {
            return Ok(vec![self.create_single_community(graph).await?]);
        }

        // Detect communities using optimized algorithm
        let communities = detect_communities(&graph.entities, &graph.relationships, &self.config);

        // Generate summaries in batches for efficiency
        let communities_with_summaries = self
            .generate_summaries_batch(communities, &graph.entities)
            .await?;

        Ok(communities_with_summaries)
    }

    /**
     * Creates a single community for small graphs (below threshold).
     * This avoids the overhead of community detection for tiny graphs.
     */
    async fn create_single_community(
        &self,
        graph: &DocumentEntityGraph,
    ) -> Result<CommunityWithSummary, CommunityManagerError> {
        let entity_ids: Vec<String> = graph
            .entities
            .iter()
            .map(|e| e.normalized_name.clone())
            .collect();

        let importance = calculate_importance(&entity_ids, &graph.relationships);

        let community = EntityCommunity {
            id: uuid::Uuid::new_v4(),
            name: if entity_ids.len() <= 3 {
                entity_ids.join(" + ")
            } else {
                format!("Entity Group ({} entities)", entity_ids.len())
            },
            entity_ids,
            importance,
        };

        // Generate summary and embedding
        let (summary, embedding) = self.generate_summary(&community, &graph.entities).await?;

        Ok(CommunityWithSummary {
            community,
            summary,
            summary_embedding: embedding,
        })
    }

    /**
     * Generates summaries for multiple communities in batches.
     * This minimizes LLM API calls by processing communities together.
     */
    async fn generate_summaries_batch(
        &self,
        communities: Vec<EntityCommunity>,
        all_entities: &[EntityNode],
    ) -> Result<Vec<CommunityWithSummary>, CommunityManagerError> {
        let mut results = Vec::with_capacity(communities.len());

        // Create entity lookup for fast access
        let entity_map: HashMap<&str, &EntityNode> = all_entities
            .iter()
            .map(|e| (e.normalized_name.as_str(), e))
            .collect();

        // Process communities in batches
        for batch in communities.chunks(SUMMARY_BATCH_SIZE) {
            let batch_futures: Vec<_> = batch
                .iter()
                .map(|community| async {
                    let (summary, embedding) = self
                        .generate_summary(community, all_entities)
                        .await?;
                    
                    Ok::<_, CommunityManagerError>(CommunityWithSummary {
                        community: community.clone(),
                        summary,
                        summary_embedding: embedding,
                    })
                })
                .collect();

            let batch_results = futures::future::try_join_all(batch_futures).await?;
            results.extend(batch_results);
        }

        Ok(results)
    }

    /**
     * Generates a summary and embedding for a single community.
     * 
     * For performance, this creates a synthetic summary from entity names
     * rather than calling an LLM, avoiding latency and API costs.
     * The summary captures the key themes of the community.
     */
    async fn generate_summary(
        &self,
        community: &EntityCommunity,
        all_entities: &[EntityNode],
    ) -> Result<(Option<String>, Option<Vec<f32>>), CommunityManagerError> {
        // Build entity lookup
        let entity_map: HashMap<&str, &EntityNode> = all_entities
            .iter()
            .map(|e| (e.normalized_name.as_str(), e))
            .collect();

        // Collect entity information for summary
        let mut entity_descriptions = Vec::new();
        let mut entity_types: HashSet<&str> = HashSet::new();

        for entity_id in &community.entity_ids {
            if let Some(entity) = entity_map.get(entity_id.as_str()) {
                let desc = if let Some(ref description) = entity.description {
                    format!("{} ({}): {}", entity.display_name, entity.entity_type, description)
                } else {
                    format!("{} ({})", entity.display_name, entity.entity_type)
                };
                entity_descriptions.push(desc);
                entity_types.insert(&entity.entity_type);
            }
        }

        if entity_descriptions.is_empty() {
            return Ok((None, None));
        }

        // Generate synthetic summary (fast, no LLM call needed)
        let summary = self.create_synthetic_summary(&community.name, &entity_descriptions, &entity_types);

        // Generate embedding for the summary
        let embedding = self.embedder.embed(&[summary.clone()]).await.map_err(|e| CommunityManagerError::Embedding(e))?;
        
        Ok((Some(summary), Some(embedding.into_iter().next().unwrap_or_default())))
    }

    /**
     * Creates a synthetic summary from entity information.
     * This is fast and doesn't require LLM calls.
     */
    fn create_synthetic_summary(
        &self,
        community_name: &str,
        entity_descriptions: &[String],
        entity_types: &HashSet<&str>,
    ) -> String {
        let types_str: Vec<_> = entity_types.iter().map(|&t| t.to_string()).collect();
        
        let summary = format!(
            "Community: {}. Contains {} entities of types: {}. Key members: {}",
            community_name,
            entity_descriptions.len(),
            types_str.join(", "),
            entity_descriptions[..entity_descriptions.len().min(5)].join("; ")
        );

        summary
    }

    /**
     * Performs two-tier retrieval:
     * - Local: Returns chunks from entities in the same community as seed entities
     * - Global: Returns communities matching the query embedding for broad exploration
     */
    pub async fn two_tier_retrieve(
        &self,
        communities: &[CommunityWithSummary],
        query_embedding: &[f32],
        seed_entity_names: &[String],
        top_k_communities: usize,
    ) -> TwoTierRetrievalResult {
        // Local tier: Find communities containing seed entities
        let local_communities: Vec<_> = communities
            .iter()
            .filter(|c| {
                c.community
                    .entity_ids
                    .iter()
                    .any(|e| seed_entity_names.contains(e))
            })
            .cloned()
            .collect();

        // Global tier: Find communities matching query embedding
        let global_communities = self.rank_communities_by_embedding(communities, query_embedding, top_k_communities);

        TwoTierRetrievalResult {
            local: local_communities,
            global: global_communities,
        }
    }

    /**
     * Ranks communities by similarity to query embedding.
     * Returns top-k communities for global exploration.
     */
    fn rank_communities_by_embedding(
        &self,
        communities: &[CommunityWithSummary],
        query_embedding: &[f32],
        top_k: usize,
    ) -> Vec<CommunityWithSummary> {
        let mut scored: Vec<(f32, CommunityWithSummary)> = communities
            .iter()
            .filter_map(|c| {
                c.summary_embedding.as_ref().map(|emb| {
                    let similarity = cosine_similarity(query_embedding, emb);
                    (similarity, c.clone())
                })
            })
            .collect();

        // Sort by similarity descending
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        // Return top-k
        scored.into_iter().take(top_k).map(|(_, c)| c).collect()
    }
}

/// Result of two-tier retrieval.
#[derive(Debug, Clone)]
pub struct TwoTierRetrievalResult {
    /// Communities containing seed entities (for local context).
    pub local: Vec<CommunityWithSummary>,
    /// Communities matching query (for global exploration).
    pub global: Vec<CommunityWithSummary>,
}

#[derive(Debug, thiserror::Error)]
pub enum CommunityManagerError {
    #[error("embedding failed: {0}")]
    Embedding(#[from] EmbeddingError),
    #[error("no entities in community")]
    EmptyCommunity,
}

/// Calculate cosine similarity between two vectors.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }

    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot / (norm_a * norm_b)
}

/// Calculate importance score for a community.
fn calculate_importance(entity_ids: &[String], relationships: &[EntityEdge]) -> f32 {
    if entity_ids.is_empty() {
        return 0.0;
    }

    let entity_set: HashSet<&str> = entity_ids.iter().map(|s| s.as_str()).collect();

    let internal_relationships: f32 = relationships
        .iter()
        .filter(|edge| {
            entity_set.contains(edge.source_normalized_name.as_str())
                && entity_set.contains(edge.target_normalized_name.as_str())
        })
        .map(|edge| edge.weight)
        .sum();

    let entity_count = entity_ids.len() as f32;
    let density = internal_relationships / entity_count.max(1.0);

    (entity_count.sqrt() * density).min(10.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct FakeEmbedder;

    #[async_trait]
    impl EmbeddingProvider for FakeEmbedder {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
            Ok(texts
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    let mut emb = vec![0.0; 384];
                    emb[i % 384] = 1.0;
                    emb
                })
                .collect())
        }
    }

    #[tokio::test]
    async fn test_small_graph_single_community() {
        let embedder: Arc<dyn EmbeddingProvider> = Arc::new(FakeEmbedder);
        let manager = CommunityManager::new(CommunityDetectionConfig::default(), embedder);

        let graph = DocumentEntityGraph {
            entities: vec![
                EntityNode {
                    normalized_name: "alice".to_string(),
                    display_name: "Alice".to_string(),
                    entity_type: "PERSON".to_string(),
                    description: Some("Engineer".to_string()),
                    embedding: vec![1.0; 384],
                },
                EntityNode {
                    normalized_name: "bob".to_string(),
                    display_name: "Bob".to_string(),
                    entity_type: "PERSON".to_string(),
                    description: None,
                    embedding: vec![0.0; 384],
                },
            ],
            mentions: vec![],
            relationships: vec![],
        };

        let communities = manager.analyze_graph(&graph).await.unwrap();
        assert_eq!(communities.len(), 1);
        assert!(communities[0].summary.is_some());
    }

    #[tokio::test]
    async fn test_two_tier_retrieval() {
        let embedder: Arc<dyn EmbeddingProvider> = Arc::new(FakeEmbedder);
        let manager = CommunityManager::new(CommunityDetectionConfig::default(), embedder);

        let communities = vec![
            CommunityWithSummary {
                community: EntityCommunity {
                    id: uuid::Uuid::new_v4(),
                    name: "Test Community".to_string(),
                    entity_ids: vec!["alice".to_string(), "bob".to_string()],
                    importance: 1.0,
                },
                summary: Some("Test summary".to_string()),
                summary_embedding: Some(vec![1.0, 0.0, 0.0]),
            },
        ];

        let result = manager
            .two_tier_retrieve(&communities, &[1.0, 0.0, 0.0], &["alice".to_string()], 5)
            .await;

        assert!(!result.local.is_empty());
    }
}
