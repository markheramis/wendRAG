/*!
 * FTS5 query building, LIKE escaping, and trigram similarity scoring used by
 * the SQLite sparse retrieval branches.
 */

use std::collections::HashMap;

/**
 * Produces a safe FTS5 query by quoting simple tokens and dropping characters
 * that would otherwise be interpreted as FTS syntax.
 */
pub(crate) fn build_fts5_query(query: &str) -> Option<String> {
    let tokens: Vec<String> = query
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect();

    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" "))
    }
}

/**
 * Escapes SQL LIKE wildcards so trigram candidate selection treats the query as
 * literal text rather than a user-provided pattern.
 */
pub(crate) fn escape_like_pattern(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/**
 * Computes Sorensen-Dice similarity on lowercase trigram multisets. This gives
 * SQLite fuzzy matching a comparable scoring shape to PostgreSQL trigram search.
 */
pub(crate) fn trigram_similarity(left: &str, right: &str) -> f64 {
    let left = left.to_lowercase();
    let right = right.to_lowercase();

    if left.is_empty() || right.is_empty() {
        return 0.0;
    }

    if left == right {
        return 1.0;
    }

    if left.chars().count() < 3 || right.chars().count() < 3 {
        return if left.contains(&right) || right.contains(&left) {
            1.0
        } else {
            0.0
        };
    }

    let left_trigrams = build_trigram_multiset(&left);
    let right_trigrams = build_trigram_multiset(&right);

    let mut intersection = 0usize;
    for (trigram, count) in &left_trigrams {
        let other_count = right_trigrams.get(trigram).copied().unwrap_or(0);
        intersection += std::cmp::min(*count, other_count);
    }

    let left_total: usize = left_trigrams.values().sum();
    let right_total: usize = right_trigrams.values().sum();
    (2.0 * intersection as f64) / (left_total + right_total) as f64
}

/**
 * Builds a trigram multiset for Sorensen-Dice scoring.
 */
fn build_trigram_multiset(value: &str) -> HashMap<String, usize> {
    let chars: Vec<char> = value.chars().collect();
    let mut counts: HashMap<String, usize> = HashMap::new();

    for window in chars.windows(3) {
        let trigram: String = window.iter().collect();
        *counts.entry(trigram).or_default() += 1;
    }

    counts
}
