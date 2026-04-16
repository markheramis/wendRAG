/**
 * Memory retrieval with hybrid semantic + recency-weighted scoring.
 *
 * Combines embedding cosine similarity with temporal proximity and importance
 * weighting. The embedder is used to generate query vectors on the fly.
 */

use std::sync::Arc;

use crate::embed::EmbeddingProvider;
use crate::memory::{
    calculate_recency_weighted_score,
    storage::{MemoryStorage, MemoryStorageError},
    types::{ChatMessage, MemoryEntry, MemoryQuery, MemoryScope, MemoryType, MessageRole},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetrievalStrategy {
    Semantic,
    RecencyWeighted,
    Hybrid,
    RecentOnly,
}

impl RetrievalStrategy {
    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "semantic" => Self::Semantic,
            "recency" | "recency_weighted" => Self::RecencyWeighted,
            "recent_only" | "recent" => Self::RecentOnly,
            _ => Self::Hybrid,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MemoryContext {
    pub session_summary: Option<String>,
    pub recent_messages: Vec<ChatMessage>,
    pub relevant_memories: Vec<MemoryEntry>,
    pub user_preferences: Vec<String>,
    pub estimated_tokens: usize,
}

impl MemoryContext {
    pub fn empty() -> Self {
        Self {
            session_summary: None,
            recent_messages: Vec::new(),
            relevant_memories: Vec::new(),
            user_preferences: Vec::new(),
            estimated_tokens: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.session_summary.is_none()
            && self.recent_messages.is_empty()
            && self.relevant_memories.is_empty()
            && self.user_preferences.is_empty()
    }

    pub fn format_for_prompt(&self) -> String {
        let mut parts = Vec::new();

        if !self.user_preferences.is_empty() {
            parts.push(format!(
                "User preferences:\n{}",
                self.user_preferences.join("\n")
            ));
        }

        if !self.relevant_memories.is_empty() {
            let texts: Vec<String> = self
                .relevant_memories
                .iter()
                .map(|m| format!("- {}", m.content))
                .collect();
            parts.push(format!("Relevant information:\n{}", texts.join("\n")));
        }

        if let Some(summary) = &self.session_summary {
            parts.push(format!("Previous conversation:\n{}", summary));
        }

        if !self.recent_messages.is_empty() {
            let msgs: Vec<String> = self
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
            parts.push(format!("Current conversation:\n{}", msgs.join("\n")));
        }

        parts.join("\n\n")
    }

    pub fn estimate_tokens(&self) -> usize {
        let prefs: usize = self.user_preferences.iter().map(|p| p.len() / 4).sum();
        let mems: usize = self.relevant_memories.iter().map(|m| m.content.len() / 4).sum();
        let summary = self.session_summary.as_ref().map(|s| s.len() / 4).unwrap_or(0);
        let msgs: usize = self.recent_messages.iter().map(|m| m.content.len() / 4).sum();
        prefs + mems + summary + msgs
    }
}

pub struct MemoryRetriever {
    storage: Arc<dyn MemoryStorage>,
    embedder: Arc<dyn EmbeddingProvider>,
}

impl MemoryRetriever {
    pub fn new(storage: Arc<dyn MemoryStorage>, embedder: Arc<dyn EmbeddingProvider>) -> Self {
        Self { storage, embedder }
    }

    /**
     * Retrieves and scores memories. Embeds the query text, then delegates to
     * the storage backend for ANN search, and applies recency weighting.
     */
    pub async fn retrieve(
        &self,
        query_text: &str,
        user_id: Option<&str>,
        limit: usize,
        recency_weight: f32,
    ) -> Result<Vec<ScoredMemory>, MemoryStorageError> {
        let query_embedding = self
            .embedder
            .embed(&[query_text.to_string()])
            .await
            .ok()
            .and_then(|mut v| v.pop());

        let mut memory_query = MemoryQuery::new()
            .with_text(query_text)
            .limit(limit * 2);

        if let Some(emb) = query_embedding {
            memory_query = memory_query.with_embedding(emb);
        }
        if let Some(uid) = user_id {
            memory_query = memory_query.for_user(uid);
        }

        let memories = self.storage.query_memories(&memory_query).await?;

        let now = chrono::Utc::now();
        let mut scored: Vec<ScoredMemory> = memories
            .into_iter()
            .map(|m| {
                let days_since = (now - m.last_accessed).num_seconds() as f32 / 86400.0;
                let relevance = 0.8 + m.importance_score * 0.2;
                let score = calculate_recency_weighted_score(relevance, days_since, recency_weight);
                ScoredMemory { memory: m, score }
            })
            .collect();

        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored)
    }

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
}

#[derive(Debug, Clone)]
pub struct ScoredMemory {
    pub memory: MemoryEntry,
    pub score: f32,
}

/**
 * Builds comprehensive context from session state and long-term memory
 * for injection into LLM prompts.
 */
pub async fn build_memory_context(
    session_summary: Option<String>,
    recent_messages: Vec<ChatMessage>,
    _storage: Arc<dyn MemoryStorage>,
    query: &str,
    user_id: Option<&str>,
    retriever: &MemoryRetriever,
    max_memories: usize,
    recency_weight: f32,
) -> Result<MemoryContext, MemoryStorageError> {
    let mut context = MemoryContext::empty();
    context.session_summary = session_summary;
    context.recent_messages = recent_messages;

    let scored = retriever
        .retrieve(query, user_id, max_memories, recency_weight)
        .await?;

    for s in scored {
        match s.memory.metadata.entry_type {
            MemoryType::Preference => {
                context.user_preferences.push(s.memory.content.clone());
            }
            _ => {
                context.relevant_memories.push(s.memory);
            }
        }
    }

    context.estimated_tokens = context.estimate_tokens();
    Ok(context)
}

#[cfg(test)]
mod tests {
    use super::*;

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
                "User prefers dark mode",
                MemoryType::Preference,
            )],
            user_preferences: vec!["User likes Rust".to_string()],
            estimated_tokens: 50,
        };

        let formatted = ctx.format_for_prompt();
        assert!(formatted.contains("User preferences"));
        assert!(formatted.contains("User likes Rust"));
        assert!(formatted.contains("Previous conversation"));
    }

    #[test]
    fn test_retrieval_strategy_parsing() {
        assert_eq!(RetrievalStrategy::from_str_loose("semantic"), RetrievalStrategy::Semantic);
        assert_eq!(RetrievalStrategy::from_str_loose("recency"), RetrievalStrategy::RecencyWeighted);
        assert_eq!(RetrievalStrategy::from_str_loose("recent_only"), RetrievalStrategy::RecentOnly);
        assert_eq!(RetrievalStrategy::from_str_loose("anything"), RetrievalStrategy::Hybrid);
    }

    #[test]
    fn test_recency_weighted_scoring() {
        use crate::memory::calculate_recency_weighted_score;
        let score1 = calculate_recency_weighted_score(0.9, 0.0, 0.3);
        assert!(score1 > 0.8);
        let score2 = calculate_recency_weighted_score(0.9, 30.0, 0.3);
        assert!(score2 < score1);
    }

    #[test]
    fn test_scored_memory_ordering() {
        let m1 = MemoryEntry::new(MemoryScope::User, None, None, "Fact 1", MemoryType::Fact);
        let m2 = MemoryEntry::new(MemoryScope::User, None, None, "Fact 2", MemoryType::Fact);

        let mut vec = vec![
            ScoredMemory { memory: m2, score: 0.5 },
            ScoredMemory { memory: m1, score: 0.9 },
        ];
        vec.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
        assert!(vec[0].score > vec[1].score);
    }
}
