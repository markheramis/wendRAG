/**
 * Memory / Session Layer for Agent-Oriented RAG
 *
 * Implements a three-tier memory system:
 * - **Session Memory:** Short-term conversation buffer (in-memory)
 * - **User Memory:** Long-term facts and preferences per user (persistent)
 * - **Global Memory:** Cross-user shared knowledge (persistent)
 *
 * Features:
 * - Decay-based importance scoring (exponential forgetting)
 * - Automatic consolidation of similar memories
 * - Hybrid retrieval (semantic + recency-weighted)
 * - Integration with existing entity graph
 *
 * Architecture inspired by the Memoria framework and cognitive psychology models.
 */

pub mod buffer;
pub mod maintenance;
pub mod manager;
pub mod pg_storage;
pub mod retrieval;
pub mod sqlite_storage;
pub mod storage;
pub mod types;

pub use buffer::{SessionBuffer, SessionConfig};
pub use manager::{MaintenanceResult, MemoryConfig, MemoryManager};
pub use pg_storage::PostgresMemoryStorage;
pub use retrieval::{MemoryContext, MemoryRetriever, RetrievalStrategy};
pub use sqlite_storage::SqliteMemoryStorage;
pub use storage::{MemoryStorage, MemoryStorageError};
pub use types::{ChatMessage, MemoryEntry, MemoryMetadata, MemoryScope, MemoryType};

use chrono::{DateTime, Utc};

/**
 * Calculate decayed importance score using exponential decay formula.
 *
 * Formula: importance * e^(-decay_rate * days_since_access)
 *
 * Based on Ebbinghaus forgetting curve and Memoria framework research.
 *
 * # Arguments
 * * `importance` - Base importance score (0.0 - 1.0)
 * * `last_accessed` - When the memory was last accessed
 * * `decay_rate` - Decay rate constant (default: 0.02)
 */
pub fn calculate_decayed_importance(
    importance: f32,
    last_accessed: DateTime<Utc>,
    decay_rate: f32,
) -> f32 {
    let now = Utc::now();
    let days_since = (now - last_accessed).num_seconds() as f32 / 86400.0;
    importance * (-decay_rate * days_since).exp()
}

/**
 * Calculate recency-weighted score combining semantic relevance and temporal proximity.
 *
 * Formula: weighted_score = (1 - recency_weight) * relevance + recency_weight * recency
 *
 * # Arguments
 * * `relevance` - Semantic similarity score (0.0 - 1.0)
 * * `days_since_access` - Days since memory was last accessed
 * * `recency_weight` - Balance between relevance and recency (0.0 - 1.0)
 */
pub fn calculate_recency_weighted_score(
    relevance: f32,
    days_since_access: f32,
    recency_weight: f32,
) -> f32 {
    // Recency decays from 1.0 over time (half-life of 30 days)
    let recency = (-0.023 * days_since_access).exp(); // ln(0.5) / 30 ≈ 0.023
    (1.0 - recency_weight) * relevance + recency_weight * recency
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn test_decay_calculation() {
        let importance = 1.0;
        let last_accessed = Utc::now() - Duration::days(30);
        let decayed = calculate_decayed_importance(importance, last_accessed, 0.02);

        // After 30 days with rate 0.02: e^(-0.02 * 30) ≈ 0.55
        assert!(decayed > 0.5 && decayed < 0.6);
    }

    #[test]
    fn test_recency_weighted_score() {
        let relevance = 0.9;
        let days_since = 10.0;
        let recency_weight = 0.3;

        let score = calculate_recency_weighted_score(relevance, days_since, recency_weight);

        // Should be between pure relevance and pure recency
        assert!(score > 0.0 && score < 1.0);
        assert!(score < relevance); // Recency pulls it down
    }

    #[test]
    fn test_decay_is_zero_after_long_time() {
        let importance = 1.0;
        let last_accessed = Utc::now() - Duration::days(365);
        let decayed = calculate_decayed_importance(importance, last_accessed, 0.02);

        // After a year, importance should be very low
        assert!(decayed < 0.1);
    }
}
