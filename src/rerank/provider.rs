/**
 * Reranker provider trait and shared types.
 *
 * Defines the async interface that all reranking backends (Cohere, Jina,
 * OpenAI-compatible cross-encoders) must implement. The trait mirrors the
 * project's `EmbeddingProvider` pattern for consistency.
 */

use async_trait::async_trait;

/// Errors that can occur during a rerank request.
#[derive(Debug, thiserror::Error)]
pub enum RerankerError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Reranker API returned error: {status} — {body}")]
    Api { status: u16, body: String },
    #[error("Reranker error: {0}")]
    Other(String),
}

/// A single reranked document with its relevance score and original position.
#[derive(Debug, Clone)]
pub struct RerankResult {
    /// Zero-based index into the original `documents` slice.
    pub index: usize,
    /// Relevance score assigned by the reranker (higher = more relevant).
    /// Scale varies by provider; consumers should rely on ordering, not
    /// absolute magnitude.
    pub relevance_score: f64,
}

/**
 * Trait for reranking providers.
 *
 * Implementations receive a query and a set of candidate documents, then
 * return the top-N documents ordered by relevance. The caller is responsible
 * for mapping `RerankResult.index` back to the original candidate list.
 *
 * # Parameters
 * - `query`: The user's search query.
 * - `documents`: Candidate document texts to rerank.
 * - `top_n`: Maximum number of results to return.
 *
 * # Returns
 * A `Vec<RerankResult>` sorted by descending `relevance_score`, with at most
 * `top_n` entries.
 */
#[async_trait]
pub trait RerankerProvider: Send + Sync {
    async fn rerank(
        &self,
        query: &str,
        documents: &[String],
        top_n: usize,
    ) -> Result<Vec<RerankResult>, RerankerError>;
}
