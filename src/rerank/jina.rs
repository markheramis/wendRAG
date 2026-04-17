/*!
 * Jina Reranker API provider.
 *
 * Implements the `RerankerProvider` trait against the Jina Reranker API
 * (`POST /v1/rerank`). Sends candidate documents along with the query and
 * returns the top-N results ordered by relevance score.
 *
 * API reference: https://jina.ai/reranker/
 */

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::provider::{RerankResult, RerankerError, RerankerProvider};

/// Default Jina Reranker API endpoint.
const DEFAULT_JINA_BASE_URL: &str = "https://api.jina.ai";

/// Jina Reranker provider.
#[derive(Debug, Clone)]
pub struct JinaReranker {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

#[derive(Serialize)]
struct JinaRerankRequest<'a> {
    query: &'a str,
    documents: &'a [String],
    model: &'a str,
    top_n: usize,
}

#[derive(Deserialize)]
struct JinaRerankResponse {
    results: Vec<JinaRerankItem>,
}

#[derive(Deserialize)]
struct JinaRerankItem {
    index: usize,
    relevance_score: f64,
}

impl JinaReranker {
    /**
     * Creates a new Jina reranker.
     *
     * # Parameters
     * - `base_url`: API base URL. Pass an empty string to use the default
     *   (`https://api.jina.ai`).
     * - `api_key`: Jina API key.
     * - `model`: Model name (e.g. `jina-reranker-v2-base-multilingual`).
     */
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        let base_url = if base_url.is_empty() {
            DEFAULT_JINA_BASE_URL.to_string()
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
impl RerankerProvider for JinaReranker {
    /**
     * Sends documents to the Jina Reranker endpoint and returns the top-N
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
        let url = format!("{base}/v1/rerank");

        let body = JinaRerankRequest {
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

        let parsed: JinaRerankResponse = resp.json().await?;

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
        let reranker = JinaReranker::new(
            String::new(),
            "test-key".into(),
            "jina-reranker-v2-base-multilingual".into(),
        );
        let result = reranker.rerank("query", &[], 5).await.unwrap();
        assert!(result.is_empty());
    }

    /**
     * Verifies that an empty base_url falls back to the default Jina endpoint.
     */
    #[test]
    fn empty_base_url_uses_default() {
        let reranker = JinaReranker::new(String::new(), "key".into(), "model".into());
        assert_eq!(reranker.base_url, DEFAULT_JINA_BASE_URL);
    }
}
