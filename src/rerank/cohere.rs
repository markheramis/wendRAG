/**
 * Cohere Rerank API provider.
 *
 * Implements the `RerankerProvider` trait against the Cohere Rerank v2 API
 * (`POST /v2/rerank`). Sends candidate documents along with the query and
 * returns the top-N results ordered by relevance score.
 *
 * API reference: https://docs.cohere.com/reference/rerank
 */

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::provider::{RerankResult, RerankerError, RerankerProvider};

/// Default Cohere Rerank API endpoint.
const DEFAULT_COHERE_BASE_URL: &str = "https://api.cohere.com";

/// Cohere Rerank v2 provider.
#[derive(Debug, Clone)]
pub struct CohereReranker {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

#[derive(Serialize)]
struct CohereRerankRequest<'a> {
    query: &'a str,
    documents: &'a [String],
    model: &'a str,
    top_n: usize,
}

#[derive(Deserialize)]
struct CohereRerankResponse {
    results: Vec<CohereRerankItem>,
}

#[derive(Deserialize)]
struct CohereRerankItem {
    index: usize,
    relevance_score: f64,
}

impl CohereReranker {
    /**
     * Creates a new Cohere reranker.
     *
     * # Parameters
     * - `base_url`: API base URL. Pass an empty string to use the default
     *   (`https://api.cohere.com`).
     * - `api_key`: Cohere API key.
     * - `model`: Model name (e.g. `rerank-v3.5`).
     */
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        let base_url = if base_url.is_empty() {
            DEFAULT_COHERE_BASE_URL.to_string()
        } else {
            base_url
        };

        Self {
            client: reqwest::Client::new(),
            base_url,
            api_key,
            model,
        }
    }
}

#[async_trait]
impl RerankerProvider for CohereReranker {
    /**
     * Sends documents to the Cohere Rerank v2 endpoint and returns the top-N
     * results sorted by descending relevance score.
     */
    async fn rerank(
        &self,
        query: &str,
        documents: &[String],
        top_n: usize,
    ) -> Result<Vec<RerankResult>, RerankerError> {
        if documents.is_empty() {
            return Ok(Vec::new());
        }

        let base = self.base_url.trim_end_matches('/');
        let url = format!("{base}/v2/rerank");

        let body = CohereRerankRequest {
            query,
            documents,
            model: &self.model,
            top_n,
        };

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(RerankerError::Api {
                status: status.as_u16(),
                body: body_text,
            });
        }

        let parsed: CohereRerankResponse = resp.json().await?;

        let results = parsed
            .results
            .into_iter()
            .map(|item| RerankResult {
                index: item.index,
                relevance_score: item.relevance_score,
            })
            .collect();

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /**
     * Verifies that an empty document list returns an empty result without
     * making any HTTP calls.
     */
    #[tokio::test]
    async fn empty_documents_returns_empty() {
        let reranker = CohereReranker::new(
            String::new(),
            "test-key".into(),
            "rerank-v3.5".into(),
        );
        let result = reranker.rerank("query", &[], 5).await.unwrap();
        assert!(result.is_empty());
    }

    /**
     * Verifies that an empty base_url falls back to the default Cohere endpoint.
     */
    #[test]
    fn empty_base_url_uses_default() {
        let reranker = CohereReranker::new(String::new(), "key".into(), "model".into());
        assert_eq!(reranker.base_url, DEFAULT_COHERE_BASE_URL);
    }
}
