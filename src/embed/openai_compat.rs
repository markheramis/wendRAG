use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::provider::{EmbeddingError, EmbeddingProvider};

const DEFAULT_BATCH_SIZE: usize = 32;

#[derive(Debug, Clone)]
pub struct OpenAiCompatProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    batch_size: usize,
}

#[derive(Serialize)]
struct EmbeddingRequest<'a> {
    input: &'a [String],
    model: &'a str,
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingDataItem>,
}

#[derive(Deserialize)]
struct EmbeddingDataItem {
    embedding: Vec<f32>,
    index: usize,
}

impl OpenAiCompatProvider {
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url,
            api_key,
            model,
            batch_size: DEFAULT_BATCH_SIZE,
        }
    }
}

#[async_trait]
impl EmbeddingProvider for OpenAiCompatProvider {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let mut all_embeddings = vec![Vec::new(); texts.len()];

        for batch_start in (0..texts.len()).step_by(self.batch_size) {
            let batch_end = (batch_start + self.batch_size).min(texts.len());
            let batch = &texts[batch_start..batch_end];

            let url = format!("{}/v1/embeddings", self.base_url.trim_end_matches('/'));
            let body = EmbeddingRequest {
                input: batch,
                model: &self.model,
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
                return Err(EmbeddingError::Api {
                    status: status.as_u16(),
                    body: body_text,
                });
            }

            let parsed: EmbeddingResponse = resp.json().await?;

            for item in parsed.data {
                let global_idx = batch_start + item.index;
                if global_idx < all_embeddings.len() {
                    all_embeddings[global_idx] = item.embedding;
                }
            }
        }

        Ok(all_embeddings)
    }
}
