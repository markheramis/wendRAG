/**
 * Ollama embedding provider for local embeddings.
 *
 * Implements the `EmbeddingProvider` trait against a local Ollama server
 * (`POST /api/embed`). Ollama provides a simple, self-hosted API for running
 * embedding models locally without external API keys.
 *
 * Default configuration:
 * - Base URL: http://localhost:11434
 * - Model: nomic-embed-text (768 dimensions)
 *
 * API reference: https://github.com/ollama/ollama/blob/main/docs/api.md#generate-embeddings
 */

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::provider::{EmbeddingError, EmbeddingProvider};

const DEFAULT_BATCH_SIZE: usize = 32;

/// Default Ollama API endpoint.
pub const DEFAULT_OLLAMA_BASE_URL: &str = "http://localhost:11434";

/// Default Ollama embedding model with 768 dimensions.
pub const DEFAULT_OLLAMA_MODEL: &str = "nomic-embed-text";

/// Default embedding dimensions for the default model.
pub const DEFAULT_OLLAMA_DIMENSIONS: usize = 768;

#[derive(Debug, Clone)]
pub struct OllamaProvider {
    client: reqwest::Client,
    base_url: String,
    model: String,
    batch_size: usize,
}

#[derive(Serialize)]
struct OllamaEmbeddingRequest<'a> {
    model: &'a str,
    input: &'a [String],
}

#[derive(Deserialize)]
struct OllamaEmbeddingResponse {
    embeddings: Vec<Vec<f32>>,
}

impl OllamaProvider {
    /**
     * Creates a new Ollama embedding provider.
     *
     * # Parameters
     * - `base_url`: Ollama server base URL. Pass an empty string to use the
     *   default (`http://localhost:11434`).
     * - `model`: Model name (e.g. `nomic-embed-text`, `mxbai-embed-large`).
     */
    pub fn new(base_url: String, model: String) -> Self {
        let base_url = if base_url.is_empty() {
            DEFAULT_OLLAMA_BASE_URL.to_string()
        } else {
            base_url
        };

        let model = if model.is_empty() {
            DEFAULT_OLLAMA_MODEL.to_string()
        } else {
            model
        };

        Self {
            client: reqwest::Client::new(),
            base_url,
            model,
            batch_size: DEFAULT_BATCH_SIZE,
        }
    }
}

#[async_trait]
impl EmbeddingProvider for OllamaProvider {
    /**
     * Sends texts to the Ollama `/api/embed` endpoint and returns embeddings.
     *
     * Ollama's embedding API accepts multiple inputs in a single request and
     * returns embeddings in the same order.
     */
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let mut all_embeddings = Vec::with_capacity(texts.len());

        for batch_start in (0..texts.len()).step_by(self.batch_size) {
            let batch_end = (batch_start + self.batch_size).min(texts.len());
            let batch = &texts[batch_start..batch_end];

            let base = self.base_url.trim_end_matches('/');
            let url = format!("{base}/api/embed");

            let body = OllamaEmbeddingRequest {
                model: &self.model,
                input: batch,
            };

            let resp = self
                .client
                .post(&url)
                .json(&body)
                .send()
                .await?;

            let status = resp.status();
            if !status.is_success() {
                let body_text = resp.text().await.unwrap_or_default();
                return Err(EmbeddingError::Api {
                    status: status.as_u16(),
                    body: body_text,
                });
            }

            let parsed: OllamaEmbeddingResponse = resp.json().await?;
            all_embeddings.extend(parsed.embeddings);
        }

        Ok(all_embeddings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /**
     * Verifies that an empty text list returns an empty result without
     * making any HTTP calls.
     */
    #[tokio::test]
    async fn empty_texts_returns_empty() {
        let provider = OllamaProvider::new(String::new(), String::new());
        let result = provider.embed(&[]).await.unwrap();
        assert!(result.is_empty());
    }

    /**
     * Verifies that an empty base_url falls back to the default Ollama endpoint.
     */
    #[test]
    fn empty_base_url_uses_default() {
        let provider = OllamaProvider::new(String::new(), "nomic-embed-text".into());
        assert_eq!(provider.base_url, DEFAULT_OLLAMA_BASE_URL);
    }

    /**
     * Verifies that an empty model falls back to the default model.
     */
    #[test]
    fn empty_model_uses_default() {
        let provider = OllamaProvider::new("http://localhost:11434".into(), String::new());
        assert_eq!(provider.model, DEFAULT_OLLAMA_MODEL);
    }

    /**
     * Verifies that custom values are preserved when provided.
     */
    #[test]
    fn custom_values_preserved() {
        let provider = OllamaProvider::new(
            "http://custom-ollama:11434".into(),
            "mxbai-embed-large".into(),
        );
        assert_eq!(provider.base_url, "http://custom-ollama:11434");
        assert_eq!(provider.model, "mxbai-embed-large");
    }
}
