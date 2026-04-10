/**
 * Document reconstruction helpers that reassemble chunk-level storage into
 * readable full-document text while trimming repeated overlap.
 */

use crate::store::models::DocumentChunk;

/// Minimum repeated suffix/prefix length required before full-context
/// reconstruction trims duplicated chunk overlap.
const MIN_RECONSTRUCTED_OVERLAP_CHARS: usize = 32;

pub(super) fn json_err(msg: &str) -> String {
    serde_json::json!({ "error": msg }).to_string()
}

/**
 * Rebuilds a readable document body from ordered stored chunks while trimming
 * repeated overlap produced by the fixed-window chunking strategy.
 */
pub(super) fn reconstruct_document_from_chunks(chunks: &[DocumentChunk]) -> String {
    let mut document = String::new();
    let mut active_section_title: Option<String> = None;

    for chunk in chunks {
        let section_title = chunk
            .section_title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty());
        let chunk_content = chunk.content.trim();

        if chunk_content.is_empty() {
            continue;
        }

        let section_changed = active_section_title.as_deref() != section_title;
        if section_changed {
            if !document.is_empty() {
                document.push_str("\n\n");
            }
            if let Some(title) = section_title {
                document.push_str("## ");
                document.push_str(title);
                document.push_str("\n\n");
            }
            active_section_title = section_title.map(ToOwned::to_owned);
        }

        append_reconstructed_chunk(&mut document, chunk_content, !section_changed);
    }

    document.trim().to_string()
}

/**
 * Appends a chunk to a reconstructed document, trimming only large repeated
 * suffix/prefix overlaps so unrelated repeated phrases are preserved.
 */
fn append_reconstructed_chunk(document: &mut String, chunk_content: &str, trim_overlap: bool) {
    if document.is_empty() || !trim_overlap {
        document.push_str(chunk_content);
        return;
    }

    let overlap_chars = find_overlap_char_count(document, chunk_content);
    if overlap_chars < MIN_RECONSTRUCTED_OVERLAP_CHARS {
        document.push_str(chunk_content);
        return;
    }

    let non_overlapping_suffix: String = chunk_content.chars().skip(overlap_chars).collect();
    document.push_str(&non_overlapping_suffix);
}

/**
 * Finds the longest exact suffix/prefix match, measured in characters, between
 * already-reconstructed content and the next stored chunk candidate.
 */
fn find_overlap_char_count(existing: &str, next: &str) -> usize {
    let existing_chars: Vec<char> = existing.chars().collect();
    let next_chars: Vec<char> = next.chars().collect();
    let max_overlap = existing_chars.len().min(next_chars.len());

    for overlap_len in (1..=max_overlap).rev() {
        if existing_chars[existing_chars.len() - overlap_len..] == next_chars[..overlap_len] {
            return overlap_len;
        }
    }

    0
}

#[cfg(test)]
mod tests {
    use super::reconstruct_document_from_chunks;
    use crate::store::models::DocumentChunk;

    /**
     * Verifies that full-context reconstruction collapses a duplicated overlap
     * segment to a single occurrence while preserving the surrounding text.
     */
    #[test]
    fn trims_large_overlap() {
        let overlap = "boundary-overlap-marker-0123456789-abcdefghij";
        let chunks = vec![
            DocumentChunk {
                content: format!("Alpha intro {overlap}"),
                chunk_index: 0,
                section_title: Some("Overview".to_string()),
            },
            DocumentChunk {
                content: format!("{overlap} and tail details"),
                chunk_index: 1,
                section_title: Some("Overview".to_string()),
            },
        ];

        let reconstructed = reconstruct_document_from_chunks(&chunks);

        assert_eq!(reconstructed.matches(overlap).count(), 1);
        assert_eq!(reconstructed.matches("## Overview").count(), 1);
        assert!(reconstructed.contains("Alpha intro"));
        assert!(reconstructed.contains("and tail details"));
    }

    /**
     * Verifies that section titles appear once per contiguous section while the
     * reconstructed document preserves boundaries between sections.
     */
    #[test]
    fn preserves_section_boundaries() {
        let chunks = vec![
            DocumentChunk {
                content: "First section body.".to_string(),
                chunk_index: 0,
                section_title: Some("Overview".to_string()),
            },
            DocumentChunk {
                content: "Second section body.".to_string(),
                chunk_index: 1,
                section_title: Some("Details".to_string()),
            },
        ];

        let reconstructed = reconstruct_document_from_chunks(&chunks);

        assert_eq!(reconstructed.matches("## Overview").count(), 1);
        assert_eq!(reconstructed.matches("## Details").count(), 1);
        assert!(reconstructed.contains("First section body."));
        assert!(reconstructed.contains("Second section body."));
    }
}
