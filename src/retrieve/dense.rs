use std::sync::Arc;

use super::ScoredChunk;
use crate::store::{SearchFilters, StorageBackend};

pub async fn search(
    storage: &Arc<dyn StorageBackend>,
    query_embedding: &[f32],
    top_k: i64,
    filters: &SearchFilters,
) -> Result<Vec<ScoredChunk>, sqlx::Error> {
    storage.search_dense(query_embedding, top_k, filters).await
}
