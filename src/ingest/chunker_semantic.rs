/**
 * Semantic chunking with Max-Min algorithm improvements.
 *
 * Implements:
 * - Hard similarity thresholds with forced splits at configurable sentence counts
 * - Garbage/boilerplate filtering before chunking
 * - Optimized embedding batch processing
 */

use std::sync::Arc;

use crate::embed::EmbeddingProvider;

use super::chunker::ChunkingError;

const TEXT_WINDOW_OVERLAP: usize = 200;
const SENTENCE_GROUP_SIZE: usize = 3;

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
 * Splits text at topic boundaries detected via embedding similarity of
 * sliding sentence windows.
 *
 * Implements Max-Min algorithm with:
 * - Hard sentence count limits (forced splits)
 * - Garbage/boilerplate pre-filtering
 * - Optimized batch processing
 */
pub(super) async fn semantic_split(
    text: &str,
    embedder: &Arc<dyn EmbeddingProvider>,
    max_chunk_chars: usize,
    threshold_percentile: f64,
    max_sentences: usize,
    filter_garbage: bool,
) -> Result<Vec<String>, ChunkingError> {
    // Step 1: Split into sentences
    let sentences = split_sentences(text);
    if sentences.len() <= 1 {
        return Ok(vec![text.to_string()]);
    }

    // Step 2: Optional garbage/boilerplate filtering
    let sentences = if filter_garbage {
        filter_garbage_sentences(sentences)
    } else {
        sentences
    };

    if sentences.is_empty() {
        return Ok(vec![text.to_string()]);
    }

    // Step 3: Build overlapping windows for embedding
    let windows = build_sentence_windows(&sentences, SENTENCE_GROUP_SIZE);
    if windows.len() <= 1 {
        return Ok(vec![sentences.join(" ")]);
    }

    // Step 4: Embed windows with optimized batch processing
    let window_texts: Vec<String> = windows.iter().map(|w| w.join(" ")).collect();
    let embeddings = embedder.embed(&window_texts).await?;

    if embeddings.len() < 2 {
        return Ok(vec![sentences.join(" ")]);
    }

    // Step 5: Compute similarities between consecutive windows
    let similarities: Vec<f32> = embeddings
        .windows(2)
        .map(|pair| cosine_similarity(&pair[0], &pair[1]))
    .collect();

    // Step 6: Find breakpoints using Max-Min algorithm with hard boundaries
    let breakpoints = find_breakpoints_maxmin(&similarities, threshold_percentile, max_sentences);

    // Step 7: Group sentences at breakpoints with hard sentence limits
    let groups = group_sentences_at_breaks_maxmin(&sentences, &breakpoints, &windows, max_sentences);

    // Step 8: Handle oversized chunks
    let mut result = Vec::new();
    for group in groups {
        let joined = group.join(" ");
        if joined.len() <= max_chunk_chars {
            if !joined.trim().is_empty() {
                result.push(joined);
            }
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
 * Filters out garbage/boilerplate sentences.
 * Removes navigation, ads, copyright notices, and very short/long sentences.
 */
fn filter_garbage_sentences(sentences: Vec<String>) -> Vec<String> {
    sentences
        .into_iter()
        .filter(|s| {
            let trimmed = s.trim();
            let len = trimmed.len();

            // Filter by length
            if len < MIN_SENTENCE_LENGTH || len > MAX_SENTENCE_LENGTH {
                return false;
            }

            // Filter by garbage patterns
            let lower = trimmed.to_lowercase();
            for pattern in GARBAGE_PATTERNS {
                if lower.contains(pattern) {
                    return false;
                }
            }

            // Filter sentences that are mostly non-alphanumeric (likely noise)
            let alphanumeric_count = trimmed.chars().filter(|c| c.is_alphanumeric()).count();
            if alphanumeric_count < len / 3 {
                return false;
            }

            true
        })
        .collect()
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
 * Max-Min algorithm: Finds breakpoints using percentile threshold AND enforces
 * hard sentence count limits.
 *
 * The algorithm ensures:
 * 1. Semantic breakpoints at low similarity points
 * 2. Forced splits when sentence count reaches max_sentences
 */
fn find_breakpoints_maxmin(
    similarities: &[f32],
    threshold_percentile: f64,
    max_sentences: usize,
) -> Vec<usize> {
    if similarities.is_empty() {
        return Vec::new();
    }

    // Find semantic threshold based on percentile
    let mut sorted = similarities.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let percentile_idx = ((sorted.len() as f64) * threshold_percentile).ceil() as usize;
    let threshold = sorted[percentile_idx.min(sorted.len() - 1)];

    // Collect all semantic breakpoints
    let mut breakpoints: Vec<usize> = similarities
        .iter()
        .enumerate()
        .filter(|(_, sim)| **sim <= threshold)
        .map(|(i, _)| i)
        .collect();

    // Add forced breakpoints at max_sentences intervals (Max-Min hard boundary)
    let forced_break_interval = max_sentences.saturating_sub(SENTENCE_GROUP_SIZE);
    if forced_break_interval > 0 {
        let mut forced_break = forced_break_interval;
        while forced_break < similarities.len() {
            // Only add if not already near an existing breakpoint
            let already_near = breakpoints.iter().any(|bp| {
                let diff = if *bp > forced_break { *bp - forced_break } else { forced_break - *bp };
                diff < SENTENCE_GROUP_SIZE
            });

            if !already_near {
                breakpoints.push(forced_break);
            }
            forced_break += forced_break_interval;
        }
    }

    breakpoints.sort_unstable();
    breakpoints.dedup();
    breakpoints
}

/**
 * Groups sentences at breakpoints with Max-Min algorithm.
 * Ensures no group exceeds max_sentences even if breakpoints don't align.
 */
fn group_sentences_at_breaks_maxmin(
    sentences: &[String],
    breakpoints: &[usize],
    windows: &[Vec<String>],
    max_sentences: usize,
) -> Vec<Vec<String>> {
    if breakpoints.is_empty() || windows.is_empty() {
        // Still respect max_sentences even without breakpoints
        return split_into_max_sentences(sentences, max_sentences);
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
            // Check if this group would exceed max_sentences
            let sentence_count = brk - start;
            if sentence_count > max_sentences {
                // Split this group into smaller chunks at max_sentences boundary
                let mut pos = start;
                while pos < brk {
                    let end = (pos + max_sentences).min(brk);
                    groups.push(sentences[pos..end].to_vec());
                    pos = end;
                }
            } else {
                groups.push(sentences[start..brk].to_vec());
            }
            start = brk;
        }
    }

    // Handle remaining sentences
    if start < sentences.len() {
        let remaining = &sentences[start..];
        groups.extend(split_into_max_sentences(remaining, max_sentences));
    }

    groups
}

/**
 * Splits a slice of sentences into chunks respecting max_sentences limit.
 */
fn split_into_max_sentences(sentences: &[String], max_sentences: usize) -> Vec<Vec<String>> {
    if sentences.len() <= max_sentences {
        return vec![sentences.to_vec()];
    }

    let mut groups = Vec::new();
    let mut pos = 0;

    while pos < sentences.len() {
        let end = (pos + max_sentences).min(sentences.len());
        groups.push(sentences[pos..end].to_vec());
        pos = end;
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
        let chunk: String = chars[pos..end].iter().collect();
        if !chunk.trim().is_empty() {
            result.push(chunk);
        }
        if end == chars.len() {
            break;
        }
        pos += step;
    }

    result
}
