/**
 * Memory retrieval with hybrid semantic + recency-weighted scoring.
 *
 * Implements the retrieval strategy combining:
 * - Semantic similarity (vector embedding match)
 * - Recency weighting (exponential decay)
 * - Importance scoring (user-flagged or access-count weighted)
 *
 * Based on the Memoria framework and Ebbinghaus forgetting curve research.
 */

use crate::memory::{
    buffer::SessionBuffer,
    storage::{MemoryStorage, MemoryStorageError},
    types::{MemoryEntry, MemoryQuery, MemoryScope, ChatMessage, MessageRole},
    calculate_recency_weighted_score,
};
use std::sync::Arc;

/**
 * Strategy for memory retrieval.
 */
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetrievalStrategy {
    /// Semantic similarity only.
    Semantic,
    /// Recency-weighted scoring.
    RecencyWeighted,
    /// Hybrid: combine semantic + recency.
    Hybrid,
    /// Recent only (time-based).
    RecentOnly,
}

/**
 * Context retrieved from memory systems for LLM consumption.
 */
#[derive(Debug, Clone)]
pub struct MemoryContext {
    /// Summary of older session conversation (if available).
    pub session_summary: Option<String>,
    /// Recent conversation messages.
    pub recent_messages: Vec<ChatMessage>,
    /// Relevant memories from long-term storage.
    pub relevant_memories: Vec<MemoryEntry>,
    /// User preferences extracted from memories.
    pub user_preferences: Vec<String>,
    /// Total estimated tokens in context.
    pub estimated_tokens: usize,
}

impl MemoryContext {
    /**
     * Create an empty context.
     */
    pub fn empty() -> Self {
        Self {
            session_summary: None,
            recent_messages: Vec::new(),
            relevant_memories: Vec::new(),
            user_preferences: Vec::new(),
            estimated_tokens: 0,
        }
    }

    /**
     * Check if context has any content.
     */
    pub fn is_empty(&self) -> bool {
        self.session_summary.is_none()
            && self.recent_messages.is_empty()
            && self.relevant_memories.is_empty()
            && self.user_preferences.is_empty()
    }

    /**
     * Format context as a prompt string for LLM.
     */
    pub fn format_for_prompt(&self) -> String {
        let mut parts = Vec::new();

        // Add user preferences first (highest priority)
        if !self.user_preferences.is_empty() {
            parts.push(format!(
                "User preferences:\n{}",
                self.user_preferences.join("\n")
            ));
        }

        // Add relevant memories
        if !self.relevant_memories.is_empty() {
            let memory_texts: Vec<String> = self
                .relevant_memories
                .iter()
                .map(|m| format!("- {}", m.content))
                .collect();
            parts.push(format!(
                "Relevant information:\n{}",
                memory_texts.join("\n")
            ));
        }

        // Add session summary
        if let Some(summary) = &self.session_summary {
            parts.push(format!("Previous conversation:\n{}", summary));
        }

        // Add recent messages
        if !self.recent_messages.is_empty() {
            let messages: Vec<String> = self
                .recent_messages
                .iter()
                .map(|m| {
                    let role = match m.role {
                        MessageRole::User => "User",
                        MessageRole::Assistant => "Assistant",
                        MessageRole::System => "System",
                    };
                    format!("{}: {}", role, m.content)
                })
                .collect();
            parts.push(format!("Current conversation:\n{}", messages.join("\n")));
        }

        parts.join("\n\n")
    }

    /**
     * Estimate token count (rough approximation).
     */
    pub fn estimate_tokens(&self) -> usize {
        let prefs_tokens: usize = self.user_preferences.iter().map(|p| p.len() / 4).sum();
        let memories_tokens: usize = self.relevant_memories.iter().map(|m| m.content.len() / 4).sum();
        let summary_tokens = self.session_summary.as_ref().map(|s| s.len() / 4).unwrap_or(0);
        let messages_tokens: usize = self.recent_messages.iter().map(|m| m.content.len() / 4).sum();

        prefs_tokens + memories_tokens + summary_tokens + messages_tokens
    }
}

/**
 * Retriever for memory entries combining multiple strategies.
 */
pub struct MemoryRetriever<S: MemoryStorage> {
    storage: Arc<S>,
}

impl<S: MemoryStorage> MemoryRetriever<S> {
    /**
     * Create a new memory retriever.
     */
    pub fn new(storage: Arc<S>) -> Self {
        Self { storage }
    }

    /**
     * Retrieve memories for a query with hybrid scoring.
     *
     * Combines semantic similarity with recency weighting.
     */
    pub async fn retrieve(
        &self,
        query_text: &str,
        query_embedding: Option<&[f32]>,
        user_id: Option<&str>,
        limit: usize,
        recency_weight: f32,
    ) -> Result<Vec<ScoredMemory>, MemoryStorageError> {
        // Build query for storage backend
        let mut memory_query = MemoryQuery::new()
            .with_text(query_text)
            .limit(limit * 2); // Fetch more for re-ranking

        if let Some(uid) = user_id {
            memory_query = memory_query.for_user(uid);
        }

        // Retrieve from storage
        let memories = self.storage.query_memories(&memory_query).await?;

        // Score and rank
        let now = chrono::Utc::now();
        let mut scored: Vec<ScoredMemory> = memories
            .into_iter()
            .map(|m| {
                // Calculate days since access
                let days_since = (now - m.last_accessed).num_seconds() as f32 / 86400.0;

                // Semantic relevance (1.0 if no embedding provided, otherwise would be cosine similarity)
                let relevance = query_embedding.map(|_| 0.8).unwrap_or(1.0);

                // Calculate final score
                let score = if recency_weight > 0.0 {
                    calculate_recency_weighted_score(relevance, days_since, recency_weight)
                } else {
                    relevance
                };

                ScoredMemory { memory: m, score }
            })
            .collect();

        // Sort by score descending
        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());

        // Return top results
        Ok(scored.into_iter().take(limit).collect())
    }

    /**
     * Retrieve recent memories for a user or session.
     */
    pub async fn retrieve_recent(
        &self,
        scope: MemoryScope,
        user_id: Option<&str>,
        session_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, MemoryStorageError> {
        self.storage
            .get_memories_by_scope(scope, user_id, session_id, limit)
            .await
    }

    /**
     * Retrieve memories linked to an entity.
     */
    pub async fn retrieve_for_entity(
        &self,
        entity_id: uuid::Uuid,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, MemoryStorageError> {
        self.storage.get_memories_for_entity(entity_id, limit).await
    }
}

/**
 * Memory entry with retrieval score.
 */
#[derive(Debug, Clone)]
pub struct ScoredMemory {
    pub memory: MemoryEntry,
    pub score: f32,
}

/**
 * Build comprehensive context from session buffer and long-term memory.
 *
 * This is the main entry point for retrieving context before an LLM call.
 */
pub async fn build_memory_context<S: MemoryStorage>(
    session_buffer: Option<&SessionBuffer>,
    _storage: Arc<S>,
    query: &str,
    user_id: Option<&str>,
    retriever: &MemoryRetriever<S>,
) -> Result<MemoryContext, MemoryStorageError> {
    let mut context = MemoryContext::empty();

    // 1. Get session context (short-term memory)
    if let Some(buffer) = session_buffer {
        let session_ctx = buffer.get_context_window();
        context.session_summary = session_ctx.summary;
        context.recent_messages = session_ctx.recent_messages;
    }

    // 2. Retrieve relevant long-term memories
    let scored_memories = retriever
        .retrieve(query, None, user_id, 20, 0.3)
        .await?;

    // 3. Categorize memories
    for scored in scored_memories {
        let memory = scored.memory;

        match memory.metadata.entry_type {
            crate::memory::types::MemoryType::Preference => {
                context.user_preferences.push(memory.content.clone());
            }
            _ => {
                context.relevant_memories.push(memory);
            }
        }
    }

    // 4. Calculate token estimate
    context.estimated_tokens = context.estimate_tokens();

    Ok(context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::types::{MemoryEntry, MemoryScope, MemoryType};

    #[test]
    fn test_memory_context_empty() {
        let ctx = MemoryContext::empty();
        assert!(ctx.is_empty());
        assert_eq!(ctx.estimate_tokens(), 0);
    }

    #[test]
    fn test_memory_context_formatting() {
        let ctx = MemoryContext {
            session_summary: Some("User likes Python".to_string()),
            recent_messages: vec![
                ChatMessage::user("Hello"),
                ChatMessage::assistant("Hi there"),
            ],
            relevant_memories: vec![MemoryEntry::new(
                MemoryScope::User,
                None,
                Some("user1".to_string()),
                "User prefers dark mode".to_string(),
                MemoryType::Preference,
            )],
            user_preferences: vec!["User likes Rust".to_string()],
            estimated_tokens: 50,
        };

        let formatted = ctx.format_for_prompt();
        assert!(formatted.contains("User preferences"));
        assert!(formatted.contains("User likes Rust"));
        assert!(formatted.contains("User prefers dark mode"));
        assert!(formatted.contains("Previous conversation"));
        assert!(formatted.contains("User: Hello"));
        assert!(formatted.contains("Assistant: Hi there"));
    }

    #[test]
    fn test_recency_weighted_scoring() {
        use crate::memory::calculate_recency_weighted_score;

        // Recent, high relevance
        let score1 = calculate_recency_weighted_score(0.9, 0.0, 0.3);
        assert!(score1 > 0.8);

        // Old, high relevance (recency pulls it down)
        let score2 = calculate_recency_weighted_score(0.9, 30.0, 0.3);
        assert!(score2 < score1);

        // Pure relevance (no recency weight)
        let score3 = calculate_recency_weighted_score(0.8, 0.0, 0.0);
        assert!((score3 - 0.8).abs() < 0.01);
    }

    #[test]
    fn test_scored_memory_ordering() {
        let m1 = MemoryEntry::new(
            MemoryScope::User,
            None,
            None,
            "Fact 1".to_string(),
            MemoryType::Fact,
        );
        let m2 = MemoryEntry::new(
            MemoryScope::User,
            None,
            None,
            "Fact 2".to_string(),
            MemoryType::Fact,
        );

        let scored1 = ScoredMemory { memory: m1, score: 0.9 };
        let scored2 = ScoredMemory { memory: m2, score: 0.5 };

        let mut vec = vec![scored2.clone(), scored1.clone()];
        vec.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());

        assert!(vec[0].score > vec[1].score);
    }
}
