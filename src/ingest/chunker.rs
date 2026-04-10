/**
 * Document chunking orchestration: selects the section extraction strategy
 * based on file type and routes oversized sections to semantic or fixed-window
 * splitting.
 */

use std::sync::Arc;

use crate::config::ChunkingStrategy;
use crate::embed::EmbeddingProvider;
use crate::embed::provider::EmbeddingError;

use super::chunker_sections::{TEXT_WINDOW_SIZE, extract_markdown_sections, extract_text_sections};
use super::chunker_semantic::{semantic_split, split_with_overlap};

const MAX_CHUNK_CHARS: usize = 4000;
const TEXT_WINDOW_OVERLAP: usize = 200;

#[derive(Debug, Clone)]
pub struct RawChunk {
    pub content: String,
    pub chunk_index: i32,
    pub section_title: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ChunkingError {
    #[error("embedding error during semantic chunking: {0}")]
    Embedding(#[from] EmbeddingError),
}

pub async fn chunk_document(
    text: &str,
    file_type: &str,
    strategy: ChunkingStrategy,
    embedder: Option<&Arc<dyn EmbeddingProvider>>,
    semantic_threshold: f64,
) -> Result<Vec<RawChunk>, ChunkingError> {
    let sections = match file_type {
        "markdown" | "url" => extract_markdown_sections(text),
        _ => extract_text_sections(text),
    };

    let max_size = match file_type {
        "markdown" | "url" => MAX_CHUNK_CHARS,
        _ => TEXT_WINDOW_SIZE,
    };

    let mut chunks = Vec::new();
    let mut idx: i32 = 0;

    for (title, body) in sections {
        let body = body.trim().to_string();
        if body.is_empty() {
            continue;
        }

        if body.len() <= max_size {
            chunks.push(RawChunk {
                content: body,
                chunk_index: idx,
                section_title: title,
            });
            idx += 1;
        } else {
            let sub_chunks =
                split_oversized(&body, max_size, strategy, embedder, semantic_threshold).await?;

            for content in sub_chunks {
                chunks.push(RawChunk {
                    content,
                    chunk_index: idx,
                    section_title: title.clone(),
                });
                idx += 1;
            }
        }
    }

    if chunks.is_empty() && !text.trim().is_empty() {
        chunks.push(RawChunk {
            content: text.chars().take(max_size).collect(),
            chunk_index: 0,
            section_title: None,
        });
    }

    Ok(chunks)
}

/**
 * Routes oversized sections to either fixed-window or semantic splitting.
 */
async fn split_oversized(
    text: &str,
    max_size: usize,
    strategy: ChunkingStrategy,
    embedder: Option<&Arc<dyn EmbeddingProvider>>,
    semantic_threshold: f64,
) -> Result<Vec<String>, ChunkingError> {
    if strategy == ChunkingStrategy::Semantic {
        if let Some(emb) = embedder {
            return semantic_split(text, emb, max_size, semantic_threshold).await;
        }
        tracing::warn!(
            "semantic chunking requested but no embedder available, falling back to fixed"
        );
    }
    Ok(split_with_overlap(text, max_size, TEXT_WINDOW_OVERLAP))
}
