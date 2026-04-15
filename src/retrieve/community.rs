/**
 * Community-augmented retrieval branch for the RRF fusion pipeline.
 *
 * Performs two-tier retrieval (local + global) through the storage layer and
 * returns scored chunks for merging with the dense, sparse, and graph branches.
 */

use std::collections::HashSet;
use std::sync::Arc;

use uuid::Uuid;

use crate::store::{SearchFilters, StorageBackend};

use super::ScoredChunk;

#[derive(Debug, thiserror::Error)]
pub enum CommunitySearchError {
    #[error("storage error: {0}")]
    Storage(#[from] sqlx::Error),
}

/**
 * Retrieves chunks via community membership (two-tier: local + global).
 *
 * 1. Local tier: find communities containing entities from seed chunks
 * 2. Global tier: ANN search over community embeddings
 * 3. Collect entity IDs from matched communities
 * 4. Fetch chunks mentioning those entities via the graph search path
 */
pub async fn search(
    storage: &Arc<dyn StorageBackend>,
    query_embedding: &[f32],
    seed_chunk_ids: &[Uuid],
    top_k: i64,
    filters: &SearchFilters,
    traversal_depth: u8,
) -> Result<Vec<ScoredChunk>, CommunitySearchError> {
    let project = filters.project.as_deref();

    let local_communities = storage
        .get_communities_for_entities(project, seed_chunk_ids)
        .await?;

    let global_communities = storage
        .search_communities_by_embedding(project, query_embedding, 5)
        .await?;

    let mut all_community_ids: HashSet<Uuid> = HashSet::new();
    for c in local_communities.iter().chain(global_communities.iter()) {
        all_community_ids.insert(c.id);
    }

    if all_community_ids.is_empty() {
        return Ok(Vec::new());
    }

    let community_ids: Vec<Uuid> = all_community_ids.into_iter().collect();
    let results = storage
        .search_graph(&community_ids, top_k, filters, traversal_depth)
        .await?;

    Ok(results)
}
