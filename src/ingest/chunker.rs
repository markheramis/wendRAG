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
    max_sentences: usize,
    filter_garbage: bool,
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
        let mut body = body.trim().to_string();
        if body.is_empty() {
            continue;
        }

        // Apply garbage filtering if enabled (before any size checks)
        if filter_garbage {
            body = filter_garbage_from_text(&body);
        }

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
                split_oversized(&body, max_size, strategy, embedder, semantic_threshold, max_sentences, filter_garbage).await?;

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
    max_sentences: usize,
    filter_garbage: bool,
) -> Result<Vec<String>, ChunkingError> {
    if strategy == ChunkingStrategy::Semantic {
        if let Some(emb) = embedder {
            return semantic_split(text, emb, max_size, semantic_threshold, max_sentences, filter_garbage).await;
        }
        tracing::warn!(
            "semantic chunking requested but no embedder available, falling back to fixed"
        );
    }
    Ok(split_with_overlap(text, max_size, TEXT_WINDOW_OVERLAP))
}

/// Minimum sentence length (in chars) to be considered meaningful.
const MIN_SENTENCE_LENGTH: usize = 10;
/// Maximum sentence length (in chars) before considering it noise.
const MAX_SENTENCE_LENGTH: usize = 1000;

/// Common boilerplate patterns to filter out.
const GARBAGE_PATTERNS: &[&str] = &[
    "click here",
    "read more",
    "learn more",
    "sign up",
    "subscribe now",
    "copyright ©",
    "all rights reserved",
    "terms of service",
    "privacy policy",
    "cookie policy",
    "advertisement",
    "sponsored",
    "share this",
    "follow us",
    "home",
    "next page",
    "previous page",
    "page 1 of",
    "loading...",
    "please wait",
];

/**
 * Filters garbage/boilerplate content from text.
 * Splits into sentences, filters out garbage sentences, and rejoins.
 */
fn filter_garbage_from_text(text: &str) -> String {
    let sentences: Vec<&str> = text.split(|c| matches!(c, '.' | '!' | '?')).collect();
    let mut filtered = Vec::new();
    
    for sentence in sentences {
        let trimmed = sentence.trim();
        if trimmed.is_empty() {
            continue;
        }
        
        let len = trimmed.len();
        
        // Filter by length
        if len < MIN_SENTENCE_LENGTH || len > MAX_SENTENCE_LENGTH {
            continue;
        }
        
        // Filter by garbage patterns
        let lower = trimmed.to_lowercase();
        let is_garbage = GARBAGE_PATTERNS.iter().any(|pattern| lower.contains(pattern));
        if is_garbage {
            continue;
        }
        
        // Filter sentences that are mostly non-alphanumeric
        let alphanumeric_count = trimmed.chars().filter(|c| c.is_alphanumeric()).count();
        if alphanumeric_count < len / 3 {
            continue;
        }
        
        filtered.push(trimmed);
    }
    
    filtered.join(". ")
}
