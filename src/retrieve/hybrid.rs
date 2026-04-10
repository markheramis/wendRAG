use std::sync::Arc;

use crate::embed::EmbeddingProvider;
use crate::embed::provider::EmbeddingError;
use crate::entity::GraphSettings;
use crate::store::{SearchFilters, StorageBackend};

use super::{ScoredChunk, dense, fusion, sparse};

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
        let seed_chunk_ids = seed_results
            .iter()
            .map(|chunk| chunk.chunk_id)
            .collect::<Vec<_>>();
        let graph_results = storage
            .search_graph(
                &seed_chunk_ids,
                top_k,
                filters,
                graph_settings.traversal_depth,
            )
            .await?;
        if !graph_results.is_empty() {
            branches.push(graph_results);
        }
    }

    Ok(fusion::reciprocal_rank_fusion(&branches, top_k as usize))
}
