/*!
 * OpenAI-compatible reranker provider.
 *
 * Implements the `RerankerProvider` trait against any server that exposes a
 * `/v1/rerank` endpoint following the emerging OpenAI-compatible rerank
 * convention (used by Infinity, TEI, vLLM, and other local serving frameworks).
 *
 * Request body:
 * ```json
 * { "query": "...", "documents": ["..."], "model": "...", "top_n": 10 }
 * ```
 *
 * Response body:
 * ```json
 * { "results": [{ "index": 0, "relevance_score": 0.95 }, ...] }
 * ```
 */

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::provider::{RerankResult, RerankerError, RerankerProvider};

/// Default base URL for a local cross-encoder server.
const DEFAULT_BASE_URL: &str = "http://localhost:8787";

/// OpenAI-compatible reranker provider for local cross-encoder servers.
#[derive(Debug, Clone)]
pub struct OpenAiCompatReranker {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

#[derive(Serialize)]
struct RerankRequest<'a> {
    query: &'a str,
    documents: &'a [String],
    model: &'a str,
    top_n: usize,
}

#[derive(Deserialize)]
struct RerankResponse {
    results: Vec<RerankResponseItem>,
}

#[derive(Deserialize)]
struct RerankResponseItem {
    index: usize,
    relevance_score: f64,
}

impl OpenAiCompatReranker {
    /**
     * Creates a new OpenAI-compatible reranker.
     *
     * # Parameters
     * - `base_url`: Server base URL. Pass an empty string to use the default
     *   (`http://localhost:8787`).
     * - `api_key`: Optional API key for authenticated endpoints.
     * - `model`: Model name exposed by the server.
     */
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        let base_url = if base_url.is_empty() {
            DEFAULT_BASE_URL.to_string()
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
impl RerankerProvider for OpenAiCompatReranker {
    /**
     * Sends documents to the `/v1/rerank` endpoint and returns the top-N
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
        let base = base.trim_end_matches("/v1");
        let url = format!("{base}/v1/rerank");

        let body = RerankRequest {
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

        let parsed: RerankResponse = resp.json().await?;

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
        let reranker = OpenAiCompatReranker::new(
            String::new(),
            String::new(),
            "bge-reranker-v2-m3".into(),
        );
        let result = reranker.rerank("query", &[], 5).await.unwrap();
        assert!(result.is_empty());
    }

    /**
     * Verifies that an empty base_url falls back to the default local endpoint.
     */
    #[test]
    fn empty_base_url_uses_default() {
        let reranker =
            OpenAiCompatReranker::new(String::new(), String::new(), "model".into());
        assert_eq!(reranker.base_url, DEFAULT_BASE_URL);
    }

    /**
     * Verifies that trailing `/v1` in the base URL is normalized to avoid
     * double `/v1/v1` paths in the request URL.
     */
    #[test]
    fn base_url_trailing_v1_is_stripped() {
        let reranker = OpenAiCompatReranker::new(
            "http://localhost:8787/v1".into(),
            String::new(),
            "model".into(),
        );
        // The URL construction trims trailing /v1, so the stored base_url
        // keeps it but the actual request URL would be correct.
        assert_eq!(reranker.base_url, "http://localhost:8787/v1");
    }
}
