use std::sync::Arc;

use crate::embed::EmbeddingProvider;
use crate::embed::provider::EmbeddingError;
use crate::entity::GraphSettings;
use crate::store::{SearchFilters, StorageBackend};

use super::{ScoredChunk, community, dense, fusion, sparse};

#[derive(Debug, thiserror::Error)]
pub enum HybridSearchError {
    #[error("query embedding failed: {0}")]
    Embedding(#[from] EmbeddingError),
    #[error("embedding provider returned no vectors for the query")]
    EmptyEmbedding,
    #[error("storage search failed: {0}")]
    Storage(#[from] sqlx::Error),
}

/**
 * Executes the hybrid retrieval pipeline by combining dense and sparse search,
 * and optionally appending a graph branch derived from the fused seed chunks.
 */
pub async fn search(
    storage: &Arc<dyn StorageBackend>,
    embedder: &Arc<dyn EmbeddingProvider>,
    query: &str,
    top_k: i64,
    filters: &SearchFilters,
    graph_settings: GraphSettings,
) -> Result<Vec<ScoredChunk>, HybridSearchError> {
    let query_owned = query.to_string();
    let embedder = embedder.clone();

    let embed_future = async move {
        let mut embeddings = embedder.embed(&[query_owned]).await?;
        embeddings.pop().ok_or(HybridSearchError::EmptyEmbedding)
    };
    let sparse_future = async {
        sparse::search(storage, query, top_k, filters)
            .await
            .map_err(HybridSearchError::from)
    };

    let (dense_embedding, sparse_results) = tokio::try_join!(embed_future, sparse_future)?;
    let dense_results = dense::search(storage, &dense_embedding, top_k, filters).await?;

    let mut branches = vec![dense_results, sparse_results];
    if graph_settings.enabled {
        let seed_results = fusion::reciprocal_rank_fusion(&branches, top_k as usize);
        let seed_chunk_ids: Vec<_> = seed_results.iter().map(|c| c.chunk_id).collect();

        // PERF-05: graph search and community search are independent
        // (they both build on the same seed chunks but hit different
        // backend queries). Running them in parallel halves the
        // post-fusion latency when both branches have work to do.
        //
        // Community search is best-effort -- treat any error as an empty
        // result set so a community-index failure never blocks the
        // primary retrieval path.
        let graph_future = storage.search_graph(
            &seed_chunk_ids,
            top_k,
            filters,
            graph_settings.traversal_depth,
        );
        let community_future = community::search(
            storage,
            &dense_embedding,
            &seed_chunk_ids,
            top_k,
            filters,
            graph_settings.traversal_depth,
        );

        let (graph_results, community_results) = tokio::join!(graph_future, community_future);
        let graph_results = graph_results?;
        let community_results = community_results.unwrap_or_default();

        if !graph_results.is_empty() {
            branches.push(graph_results);
        }
        if !community_results.is_empty() {
            branches.push(community_results);
        }
    }

    Ok(fusion::reciprocal_rank_fusion(&branches, top_k as usize))
}
