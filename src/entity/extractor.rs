/*!
 * OpenAI-compatible chat-completions client used for optional entity
 * extraction during the ingestion pipeline.
 */

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use super::model::{
    ChunkEntityExtraction, EntityExtractionError, EntityExtractionInput, EntityExtractor,
    ExtractedEntity, ExtractedRelationship,
};
use super::normalize::{
    clean_optional_text, clean_required_text, default_entity_type, normalize_entity_type,
};

const EXTRACTION_SYSTEM_PROMPT: &str = "Extract named entities and explicit relationships from the provided chunk. Return JSON only with this shape: {\"entities\":[{\"name\":\"...\",\"entity_type\":\"PERSON|ORG|CONCEPT|LOCATION|TECHNOLOGY|SERVICE|TEAM\",\"description\":\"optional short description\"}],\"relationships\":[{\"source_name\":\"...\",\"source_type\":\"...\",\"target_name\":\"...\",\"target_type\":\"...\",\"relationship_type\":\"...\",\"description\":\"optional short explanation\",\"weight\":1.0}]}. Use only entities grounded in the chunk. Keep relationship_type concise and uppercase snake case when possible.";

/** OpenAI-compatible chat-completions client used for optional entity extraction. */
#[derive(Debug, Clone)]
pub struct OpenAiCompatEntityExtractor {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    content: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ExtractionPayload {
    #[serde(default)]
    entities: Vec<ExtractionEntityPayload>,
    #[serde(default)]
    relationships: Vec<ExtractionRelationshipPayload>,
}

#[derive(Debug, Deserialize)]
struct ExtractionEntityPayload {
    name: String,
    entity_type: Option<String>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ExtractionRelationshipPayload {
    source_name: String,
    source_type: Option<String>,
    target_name: String,
    target_type: Option<String>,
    relationship_type: String,
    description: Option<String>,
    weight: Option<f32>,
}

impl OpenAiCompatEntityExtractor {
    /**
     * Builds the OpenAI-compatible extractor client used during optional
     * ingestion-time entity extraction.
     */
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url,
            api_key,
            model,
        }
    }
}

#[async_trait]
impl EntityExtractor for OpenAiCompatEntityExtractor {
    /**
     * Extracts entities and relationships for one chunk by calling an
     * OpenAI-compatible chat completion endpoint and parsing the JSON payload.
     */
    async fn extract(
        &self,
        input: EntityExtractionInput<'_>,
    ) -> Result<ChunkEntityExtraction, EntityExtractionError> {
        let url = format!(
            "{}/v1/chat/completions",
            self.base_url.trim_end_matches('/')
        );
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({
                "model": self.model,
                "temperature": 0.0,
                "response_format": { "type": "json_object" },
                "messages": [
                    {
                        "role": "system",
                        "content": EXTRACTION_SYSTEM_PROMPT,
                    },
                    {
                        "role": "user",
                        "content": build_extraction_prompt(input),
                    }
                ]
            }))
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(EntityExtractionError::Api {
                status: status.as_u16(),
                body,
            });
        }

        let parsed: ChatCompletionResponse = response.json().await?;
        let content = parsed
            .choices
            .first()
            .and_then(|choice| choice.message.content.as_ref())
            .and_then(extract_message_text)
            .ok_or(EntityExtractionError::MissingContent)?;

        let payload: ExtractionPayload = serde_json::from_str(&content)?;
        Ok(map_payload_to_chunk_extraction(input.chunk_index, payload))
    }
}

/**
 * Builds the LLM prompt for a single chunk while keeping the extraction task
 * grounded in the current file, section, and body text.
 */
fn build_extraction_prompt(input: EntityExtractionInput<'_>) -> String {
    format!(
        "File name: {}\nFile type: {}\nChunk index: {}\nSection title: {}\n\nChunk content:\n{}",
        input.file_name,
        input.file_type,
        input.chunk_index,
        input.section_title.unwrap_or("(none)"),
        input.content,
    )
}

/**
 * Extracts a plain text message from either a simple string response or the
 * array-of-parts format used by some OpenAI-compatible providers.
 */
fn extract_message_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let text: String = parts.iter().filter_map(|part: &Value| part.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("");
            if text.is_empty() { None } else { Some(text) }
        }
        _ => None,
    }
}

/**
 * Maps the extractor JSON payload into the chunk-scoped in-memory model used by
 * the ingestion pipeline and tests.
 */
fn map_payload_to_chunk_extraction(
    chunk_index: i32,
    payload: ExtractionPayload,
) -> ChunkEntityExtraction {
    let default_type = default_entity_type();

    // map entities from payload to ExtractedEntity
    // Convert ExtractionEntityPayloads from the extractor JSON payload into
    // the chunk-level ExtractedEntity structs. We validate and normalize
    // fields as in the entity mapping above: skip entities lacking required
    // fields, standardize types using normalize_entity_type, apply sensible
    // defaults, and clean up optional text. This ensures the graph-building code
    // downstream receives well-formed entities only.
    let entities: Vec<ExtractedEntity> = payload.entities.into_iter().filter_map(|entity: ExtractionEntityPayload| {
        let name = clean_required_text(entity.name)?;
        Some(ExtractedEntity {
            name,
            entity_type: normalize_entity_type(
                entity.entity_type.as_deref().unwrap_or(default_type),
            ),
            description: clean_optional_text(entity.description.as_deref()),
        })
    }).collect();

    // map relationships from payload to ExtractedRelationship
    // Convert ExtractionRelationshipPayloads from the extractor JSON payload into
    // the chunk-level ExtractedRelationship structs. We validate and normalize
    // fields as in the entity mapping above: skip relationships lacking required
    // fields, standardize types using normalize_entity_type, apply sensible
    // defaults, and clean up optional text. This ensures the graph-building code
    // downstream receives well-formed relationships only.
    let relationships: Vec<ExtractedRelationship> = payload.relationships.into_iter().filter_map(|relationship: ExtractionRelationshipPayload| {
            let source_name = clean_required_text(relationship.source_name)?;
            let target_name = clean_required_text(relationship.target_name)?;
            let relationship_type = clean_required_text(relationship.relationship_type)?;
            Some(ExtractedRelationship {
                source_name,
                source_type: normalize_entity_type(
                    relationship.source_type.as_deref().unwrap_or(default_type),
                ),
                target_name,
                target_type: normalize_entity_type(
                    relationship.target_type.as_deref().unwrap_or(default_type),
                ),
                relationship_type,
                description: clean_optional_text(relationship.description.as_deref()),
                weight: relationship.weight.unwrap_or(1.0).max(0.1),
            })
        }).collect();
    ChunkEntityExtraction {
        chunk_index,
        entities,
        relationships,
    }
}
