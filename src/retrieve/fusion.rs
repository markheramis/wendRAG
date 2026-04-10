use std::collections::HashMap;
use uuid::Uuid;

use super::ScoredChunk;

const DEFAULT_K: f64 = 60.0;

/// Reciprocal Rank Fusion: merge multiple ranked result lists into one.
///
/// For each unique chunk across all branches:
///   rrf_score = sum( 1.0 / (k + rank_in_branch_i) )
///
/// Chunks appearing in multiple branches get a natural boost.
pub fn reciprocal_rank_fusion(branches: &[Vec<ScoredChunk>], top_k: usize) -> Vec<ScoredChunk> {
    rrf_with_k(branches, DEFAULT_K, top_k)
}

pub fn rrf_with_k(branches: &[Vec<ScoredChunk>], k: f64, top_k: usize) -> Vec<ScoredChunk> {
    let mut scores: HashMap<Uuid, f64> = HashMap::new();
    let mut chunk_data: HashMap<Uuid, ScoredChunk> = HashMap::new();

    for branch in branches {
        for (rank, chunk) in branch.iter().enumerate() {
            let rrf_contrib = 1.0 / (k + (rank + 1) as f64);
            *scores.entry(chunk.chunk_id).or_default() += rrf_contrib;
            chunk_data
                .entry(chunk.chunk_id)
                .or_insert_with(|| chunk.clone());
        }
    }

    let mut fused: Vec<ScoredChunk> = chunk_data
        .into_iter()
        .map(|(id, mut chunk)| {
            chunk.score = scores.get(&id).copied().unwrap_or(0.0);
            chunk
        })
        .collect();

    fused.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    fused.truncate(top_k);
    fused
}
