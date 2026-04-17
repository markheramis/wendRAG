/*!
 * Memory manager — main orchestrator for the memory subsystem.
 *
 * Coordinates session buffers (in-memory), persistent long-term storage,
 * decay/maintenance operations, and embedding generation for semantic search.
 * Accepts `Arc<dyn MemoryStorage>` so it can be stored as a field in the
 * MCP server without generic parameters.
 */

use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::embed::EmbeddingProvider;
use crate::memory::{
    MemoryScope,
    buffer::{SessionBuffer, SessionConfig, SessionContext},
    calculate_decayed_importance,
    retrieval::MemoryRetriever,
    storage::{MemoryStorage, MemoryStorageError},
    types::{MemoryEntry, MemoryType},
};

#[derive(Debug, Clone)]
pub struct MemoryConfig {
    pub enabled: bool,
    pub session: SessionConfig,
    pub default_importance: f32,
    pub decay_rate: f32,
    pub prune_threshold: f32,
    pub consolidation_interval_hours: i64,
    pub session_timeout_seconds: i64,
    pub max_memories_per_query: usize,
    pub recency_weight: f32,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            session: SessionConfig::default(),
            default_importance: 0.5,
            decay_rate: 0.02,
            prune_threshold: 0.3,
            consolidation_interval_hours: 24,
            session_timeout_seconds: 3600,
            max_memories_per_query: 20,
            recency_weight: 0.3,
        }
    }
}

pub struct MemoryManager {
    config: MemoryConfig,
    storage: Arc<dyn MemoryStorage>,
    embedder: Arc<dyn EmbeddingProvider>,
    session_buffers: DashMap<String, RwLock<SessionBuffer>>,
    retriever: MemoryRetriever,
}

impl MemoryManager {
    pub fn new(
        config: MemoryConfig,
        storage: Arc<dyn MemoryStorage>,
        embedder: Arc<dyn EmbeddingProvider>,
    ) -> Self {
        let retriever = MemoryRetriever::new(Arc::clone(&storage), Arc::clone(&embedder));
        Self {
            config,
            storage,
            embedder,
            session_buffers: DashMap::new(),
            retriever,
        }
    }

    pub async fn get_session_context(&self, session_id: &str) -> Option<SessionContext> {
        self.session_buffers.get(session_id).and_then(|entry| {
            entry
                .value()
                .try_read()
                .ok()
                .map(|guard| guard.get_context())
        })
    }

    pub async fn end_session(
        &self,
        session_id: &str,
        persist_to_memory: bool,
        user_id: Option<&str>,
    ) -> Result<Option<MemoryEntry>, MemoryStorageError> {
        let buffer_opt = self.session_buffers.remove(session_id);

        if let Some((_, buffer_lock)) = buffer_opt {
            let buffer = buffer_lock.read().await;

            if persist_to_memory {
                let summary_text = buffer
                    .summary
                    .clone()
                    .unwrap_or_else(|| format!("Session with {} messages", buffer.message_count()));

                let content = format!(
                    "Session {} summary: {}\nRecent: {}",
                    session_id,
                    summary_text,
                    buffer
                        .get_recent_messages(3)
                        .iter()
                        .map(|m| format!("[{}]: {}", m.role.as_str(), m.content))
                        .collect::<Vec<_>>()
                        .join("; ")
                );

                let memory = self
                    .store_memory(
                        MemoryScope::User,
                        Some(session_id.to_string()),
                        user_id.map(|s| s.to_string()),
                        content,
                        MemoryType::Summary,
                        self.config.default_importance * 1.2,
                    )
                    .await?;

                tracing::info!(session_id, memory_id = %memory.id, "persisted session to memory");
                return Ok(Some(memory));
            }
        }

        Ok(None)
    }

    pub async fn cleanup_expired_sessions(&self) -> usize {
        let mut expired = Vec::new();
        for entry in self.session_buffers.iter() {
            if let Ok(buffer) = entry.value().try_read()
                && buffer.is_expired(self.config.session_timeout_seconds)
            {
                expired.push(entry.key().clone());
            }
        }

        let count = expired.len();
        for session_id in expired {
            self.session_buffers.remove(&session_id);
            tracing::debug!(session_id, "removed expired session");
        }
        count
    }

    /** Lists active session IDs for the MCP sessions tool. */
    pub fn list_sessions(&self) -> Vec<String> {
        self.session_buffers
            .iter()
            .map(|e| e.key().clone())
            .collect()
    }

    /**
     * Stores a memory entry, optionally generating an embedding for
     * semantic search.
     */
    pub async fn store_memory(
        &self,
        scope: MemoryScope,
        session_id: impl Into<Option<String>>,
        user_id: impl Into<Option<String>>,
        content: impl Into<String>,
        entry_type: MemoryType,
        importance: f32,
    ) -> Result<MemoryEntry, MemoryStorageError> {
        let content_str = content.into();

        let embedding = self
            .embedder
            .embed(std::slice::from_ref(&content_str))
            .await
            .ok()
            .and_then(|mut v| v.pop());

        let mut entry = MemoryEntry::new(
            scope,
            session_id.into(),
            user_id.into(),
            &content_str,
            entry_type,
        );
        entry.importance_score = importance.clamp(0.0, 1.0);
        entry.embedding = embedding;

        self.storage.store_memory(&entry).await?;
        tracing::debug!(id = %entry.id, scope = ?scope, "stored memory");
        Ok(entry)
    }

    pub async fn delete_memory(&self, id: Uuid) -> Result<bool, MemoryStorageError> {
        self.storage.delete_memory(id).await
    }

    pub async fn invalidate_memory(&self, id: Uuid) -> Result<bool, MemoryStorageError> {
        self.storage.invalidate_memory(id).await
    }

    pub async fn retrieve_memories(
        &self,
        query: &str,
        user_id: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<MemoryEntry>, MemoryStorageError> {
        let limit = limit.unwrap_or(self.config.max_memories_per_query);
        let scored = self
            .retriever
            .retrieve(query, user_id, limit, self.config.recency_weight)
            .await?;
        Ok(scored.into_iter().map(|s| s.memory).collect())
    }

    pub async fn run_maintenance(&self) -> Result<MaintenanceResult, MemoryStorageError> {
        let mut result = MaintenanceResult::default();

        result.expired_sessions_cleaned = self.cleanup_expired_sessions().await;

        let storage_stats = self.storage.run_maintenance().await?;
        result.entries_pruned += storage_stats.entries_pruned;

        result.entries_decayed = self.apply_decay_to_memories().await?;

        tracing::info!(
            sessions = result.expired_sessions_cleaned,
            pruned = result.entries_pruned,
            decayed = result.entries_decayed,
            "memory maintenance complete"
        );

        Ok(result)
    }

    async fn apply_decay_to_memories(&self) -> Result<usize, MemoryStorageError> {
        let threshold = self.config.prune_threshold * 1.5;
        let entries = self
            .storage
            .get_memories_for_pruning(threshold, 1000)
            .await?;

        let mut decayed = 0;
        for mut entry in entries {
            let old = entry.importance_score;
            let new = calculate_decayed_importance(old, entry.last_accessed, self.config.decay_rate);
            if (old - new).abs() > 0.01 {
                entry.importance_score = new;
                self.storage.update_memory(&entry).await?;
                decayed += 1;
            }
        }
        Ok(decayed)
    }

    pub async fn get_stats(&self) -> MemoryStats {
        let total = self.storage.count_memories().await.unwrap_or(0);
        let by_scope = self.storage.count_memories_by_scope().await.unwrap_or_default();
        MemoryStats {
            active_sessions: self.session_buffers.len(),
            total_memories: total,
            memories_by_scope: by_scope,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct MaintenanceResult {
    pub expired_sessions_cleaned: usize,
    pub entries_pruned: usize,
    pub entries_decayed: usize,
}

#[derive(Debug, Clone)]
pub struct MemoryStats {
    pub active_sessions: usize,
    pub total_memories: u64,
    pub memories_by_scope: Vec<(String, u64)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_config_default() {
        let config = MemoryConfig::default();
        assert!(!config.enabled);
        assert!(config.decay_rate > 0.0);
        assert!(config.prune_threshold > 0.0);
    }

    #[test]
    fn test_maintenance_result_default() {
        let result = MaintenanceResult::default();
        assert_eq!(result.expired_sessions_cleaned, 0);
        assert_eq!(result.entries_pruned, 0);
        assert_eq!(result.entries_decayed, 0);
    }
}
