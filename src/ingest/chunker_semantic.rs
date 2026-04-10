/**
 * Semantic and fixed-window text splitting strategies used to break oversized
 * sections into appropriately sized chunks.
 */

use std::sync::Arc;

use crate::embed::EmbeddingProvider;

use super::chunker::ChunkingError;

const TEXT_WINDOW_OVERLAP: usize = 200;
const SENTENCE_GROUP_SIZE: usize = 3;

/**
 * Splits text at topic boundaries detected via embedding similarity of
 * sliding sentence windows.
 */
pub(super) async fn semantic_split(
    text: &str,
    embedder: &Arc<dyn EmbeddingProvider>,
    max_chunk_chars: usize,
    threshold_percentile: f64,
) -> Result<Vec<String>, ChunkingError> {
    let sentences = split_sentences(text);
    if sentences.len() <= 1 {
        return Ok(vec![text.to_string()]);
    }

    let windows = build_sentence_windows(&sentences, SENTENCE_GROUP_SIZE);
    if windows.len() <= 1 {
        return Ok(vec![text.to_string()]);
    }

    let window_texts: Vec<String> = windows.iter().map(|w| w.join(" ")).collect();
    let embeddings = embedder.embed(&window_texts).await?;

    if embeddings.len() < 2 {
        return Ok(vec![text.to_string()]);
    }

    let similarities: Vec<f32> = embeddings
        .windows(2)
        .map(|pair| cosine_similarity(&pair[0], &pair[1]))
        .collect();

    let breakpoints = find_breakpoints(&similarities, threshold_percentile);

    let groups = group_sentences_at_breaks(&sentences, &breakpoints, &windows);

    let mut result = Vec::new();
    for group in groups {
        let joined = group.join(" ");
        if joined.len() <= max_chunk_chars {
            result.push(joined);
        } else {
            result.extend(split_with_overlap(
                &joined,
                max_chunk_chars,
                TEXT_WINDOW_OVERLAP,
            ));
        }
    }

    Ok(result)
}

/**
 * Splits text into sentences using punctuation boundary heuristics with basic
 * abbreviation handling.
 */
fn split_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();

    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        current.push(chars[i]);

        if matches!(chars[i], '.' | '!' | '?') {
            if is_abbreviation(&current) {
                i += 1;
                continue;
            }

            let next_is_boundary = if i + 1 < len {
                chars[i + 1].is_whitespace()
                    && (i + 2 >= len || chars[i + 2].is_uppercase() || chars[i + 2] == '"')
            } else {
                true
            };

            if next_is_boundary {
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    sentences.push(trimmed);
                }
                current.clear();
                if i + 1 < len && chars[i + 1].is_whitespace() {
                    i += 1;
                }
            }
        }

        i += 1;
    }

    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        sentences.push(trimmed);
    }

    if sentences.is_empty() && !text.trim().is_empty() {
        sentences.push(text.trim().to_string());
    }

    sentences
}

fn is_abbreviation(text: &str) -> bool {
    let lower = text.to_lowercase();
    let abbrevs = [
        "mr.", "mrs.", "ms.", "dr.", "prof.", "sr.", "jr.", "e.g.", "i.e.", "vs.", "etc.",
        "approx.", "dept.", "est.", "inc.", "ltd.", "no.", "vol.", "fig.",
    ];
    abbrevs.iter().any(|a| lower.ends_with(a))
}

/**
 * Groups consecutive sentences into overlapping windows for embedding.
 */
fn build_sentence_windows(sentences: &[String], group_size: usize) -> Vec<Vec<String>> {
    if sentences.len() <= group_size {
        return vec![sentences.to_vec()];
    }

    sentences.windows(group_size).map(|w| w.to_vec()).collect()
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

/**
 * Finds indices where similarity is in the bottom `threshold_percentile` of
 * all scores, indicating topic transition boundaries.
 */
fn find_breakpoints(similarities: &[f32], threshold_percentile: f64) -> Vec<usize> {
    if similarities.is_empty() {
        return Vec::new();
    }

    let mut sorted = similarities.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let percentile_idx = ((sorted.len() as f64) * threshold_percentile).ceil() as usize;
    let threshold = sorted[percentile_idx.min(sorted.len() - 1)];

    similarities
        .iter()
        .enumerate()
        .filter(|(_, sim)| **sim <= threshold)
        .map(|(i, _)| i)
        .collect()
}

/**
 * Groups original sentences into chunks, splitting at the detected breakpoints.
 * Each breakpoint index `i` means: break after sentence window `i`, which
 * corresponds to breaking after sentence `i + SENTENCE_GROUP_SIZE - 1` in the
 * original sentence list (center of the window).
 */
fn group_sentences_at_breaks(
    sentences: &[String],
    breakpoints: &[usize],
    windows: &[Vec<String>],
) -> Vec<Vec<String>> {
    if breakpoints.is_empty() || windows.is_empty() {
        return vec![sentences.to_vec()];
    }

    let half = SENTENCE_GROUP_SIZE / 2;
    let mut break_sentence_indices: Vec<usize> = breakpoints
        .iter()
        .map(|&bp| (bp + half + 1).min(sentences.len()))
        .collect();
    break_sentence_indices.sort_unstable();
    break_sentence_indices.dedup();

    let mut groups: Vec<Vec<String>> = Vec::new();
    let mut start = 0;

    for &brk in &break_sentence_indices {
        if brk > start && brk <= sentences.len() {
            groups.push(sentences[start..brk].to_vec());
            start = brk;
        }
    }

    if start < sentences.len() {
        groups.push(sentences[start..].to_vec());
    }

    groups
}

/**
 * Splits text into fixed-size character windows with configurable overlap.
 */
pub(super) fn split_with_overlap(text: &str, window: usize, overlap: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let step = window.saturating_sub(overlap).max(1);
    let mut result = Vec::new();
    let mut pos = 0;

    while pos < chars.len() {
        let end = (pos + window).min(chars.len());
        result.push(chars[pos..end].iter().collect());
        if end == chars.len() {
            break;
        }
        pos += step;
    }

    result
}
