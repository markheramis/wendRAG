use std::sync::Arc;

use super::ScoredChunk;
use crate::store::{SearchFilters, StorageBackend};

pub async fn search(
    storage: &Arc<dyn StorageBackend>,
    query: &str,
    top_k: i64,
    filters: &SearchFilters,
) -> Result<Vec<ScoredChunk>, sqlx::Error> {
    storage.search_sparse(query, top_k, filters).await
}
