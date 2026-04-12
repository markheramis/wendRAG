/**
 * Memory manager - main orchestrator for the memory subsystem.
 *
 * Coordinates:
 * - Session buffers (in-memory, per-session short-term memory)
 * - Persistent storage (long-term user/global memories)
 * - Decay and maintenance operations
 * - Integration with embedding providers for semantic search
 *
 * This is the primary interface that other components use to interact
 * with the memory system.
 */

use crate::memory::{
    MemoryScope, buffer::{SessionBuffer, SessionConfig, SessionContext}, calculate_decayed_importance, retrieval::{MemoryContext, MemoryRetriever, build_memory_context}, storage::{MemoryStorage, MemoryStorageError}, types::{ChatMessage, MemoryEntry, MemoryType, MessageRole}
};
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};
use uuid::Uuid;

/**
 * Configuration for the memory manager.
 */
#[derive(Debug, Clone)]
pub struct MemoryConfig {
    /// Enable the memory subsystem.
    pub enabled: bool,
    /// Configuration for session buffers.
    pub session: SessionConfig,
    /// Default importance score for new memories.
    pub default_importance: f32,
    /// Decay rate for importance scores (alpha in e^(-alpha * days)).
    pub decay_rate: f32,
    /// Prune memories below this importance threshold.
    pub prune_threshold: f32,
    /// Run consolidation every N hours.
    pub consolidation_interval_hours: i64,
    /// Max inactive seconds before a session expires.
    pub session_timeout_seconds: i64,
    /// Maximum memories to retrieve per query.
    pub max_memories_per_query: usize,
    /// Weight for recency in retrieval (0.0 = relevance only, 1.0 = recency only).
    pub recency_weight: f32,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            session: SessionConfig::default(),
            default_importance: 0.5,
            decay_rate: 0.02,
            prune_threshold: 0.3,
            consolidation_interval_hours: 24,
            session_timeout_seconds: 3600, // 1 hour
            max_memories_per_query: 20,
            recency_weight: 0.3,
        }
    }
}

impl MemoryConfig {
    /**
     * Create a minimal configuration for testing.
     */
    pub fn minimal() -> Self {
        Self {
            enabled: true,
            session: SessionConfig::minimal(),
            default_importance: 0.5,
            decay_rate: 0.05, // Faster decay for testing
            prune_threshold: 0.2,
            consolidation_interval_hours: 1,
            session_timeout_seconds: 60, // 1 minute
            max_memories_per_query: 5,
            recency_weight: 0.3,
        }
    }
}

/**
 * Memory manager coordinating all memory subsystems.
 */
pub struct MemoryManager<S: MemoryStorage> {
    /// Configuration for memory behavior.
    config: MemoryConfig,
    /// Storage backend for persistent memories.
    storage: Arc<S>,
    /// In-memory session buffers (session_id -> buffer).
    session_buffers: DashMap<String, RwLock<SessionBuffer>>,
    /// Retriever for long-term memories.
    retriever: MemoryRetriever<S>,
}

impl<S: MemoryStorage> MemoryManager<S> {
    /**
     * Create a new memory manager.
     */
    pub fn new(config: MemoryConfig, storage: Arc<S>) -> Self {
        let retriever = MemoryRetriever::new(Arc::clone(&storage));

        Self {
            config,
            storage,
            session_buffers: DashMap::new(),
            retriever,
        }
    }

    /**
     * Check if memory system is enabled.
     */
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    // =========================================================================
    // Session Management
    // =========================================================================

    /**
     * Get or create a session buffer and execute a closure with write access.
     *
     * This pattern avoids lifetime issues with DashMap references.
     */
    async fn with_session_buffer<F, R>(&self, session_id: &str, f: F) -> Result<R, MemoryStorageError>
    where
        F: FnOnce(&mut SessionBuffer) -> R,
    {
        // Ensure session exists
        if !self.session_buffers.contains_key(session_id) {
            let buffer = SessionBuffer::new(session_id, self.config.session.clone());
            self.session_buffers
                .insert(session_id.to_string(), RwLock::new(buffer));
            info!("Created new session buffer: {}", session_id);
        }

        // Get the lock guard
        let entry = self
            .session_buffers
            .get(session_id)
            .ok_or_else(|| MemoryStorageError::NotFound(Uuid::new_v4()))?;

        let mut guard = entry.value().write().await;
        let result = f(&mut guard);
        Ok(result)
    }

    /**
     * Get session context without holding a lock.
     */
    pub async fn get_session_context(&self, session_id: &str) -> Option<SessionContext> {
        self.session_buffers
            .get(session_id)
            .and_then(|entry| {
                entry.value().try_read().ok().map(|guard| guard.get_context())
            })
    }

    /**
     * Add a message to a session.
     *
     * Returns true if the session should be summarized.
     */
    pub async fn add_session_message(
        &self,
        session_id: &str,
        role: MessageRole,
        content: impl Into<String>,
    ) -> Result<bool, MemoryStorageError> {
        let content_str = content.into();
        self.with_session_buffer(session_id, |buffer| {
            let message = ChatMessage::new(role, content_str);
            buffer.add_message(message);
            buffer.apply_sliding_window();
            buffer.message_count() >= buffer.config.max_messages
        }).await
    }

    /**
     * Summarize a session's older messages.
     */
    pub async fn summarize_session(
        &self,
        session_id: &str,
        summary: impl Into<String>,
    ) -> Result<(), MemoryStorageError> {
        let summary_str = summary.into();
        self.with_session_buffer(session_id, |buffer| {
            buffer.summarize(summary_str);
        }).await
    }

    /**
     * End a session and optionally persist to long-term memory.
     */
    pub async fn end_session(
        &self,
        session_id: &str,
        persist_to_memory: bool,
        user_id: Option<&str>,
    ) -> Result<Option<MemoryEntry>, MemoryStorageError> {
        // Remove the session buffer
        let buffer_opt = self.session_buffers.remove(session_id);

        if let Some((_, buffer_lock)) = buffer_opt {
            let buffer = buffer_lock.read().await;

            if persist_to_memory {
                // Create a memory entry from session summary
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
                        self.config.default_importance * 1.2, // Summaries are more important
                    )
                    .await?;

                info!("Persisted session {} to memory: {}", session_id, memory.id);
                return Ok(Some(memory));
            }
        }

        Ok(None)
    }

    /**
     * Clean up expired sessions.
     */
    pub async fn cleanup_expired_sessions(&self) -> usize {
        let mut expired = Vec::new();

        for entry in self.session_buffers.iter() {
            if let Ok(buffer) = entry.value().try_read() {
                if buffer.is_expired(self.config.session_timeout_seconds) {
                    expired.push(entry.key().clone());
                }
            }
        }

        let count = expired.len();
        for session_id in expired {
            self.session_buffers.remove(&session_id);
            debug!("Removed expired session: {}", session_id);
        }

        if count > 0 {
            info!("Cleaned up {} expired sessions", count);
        }

        count
    }

    // =========================================================================
    // Long-Term Memory Operations
    // =========================================================================

    /**
     * Store a new memory entry.
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
        let mut entry = MemoryEntry::new(
            scope,
            session_id.into(),
            user_id.into(),
            content_str,
            entry_type,
        );
        entry.importance_score = importance.clamp(0.0, 1.0);

        self.storage.store_memory(&entry).await?;
        debug!("Stored memory: {} (scope: {:?})", entry.id, scope);

        Ok(entry)
    }

    /**
     * Store a fact about the user.
     */
    pub async fn store_user_fact(
        &self,
        user_id: &str,
        fact: impl Into<String>,
        importance: f32,
    ) -> Result<MemoryEntry, MemoryStorageError> {
        self.store_memory(
            MemoryScope::User,
            None::<String>,
            Some(user_id.to_string()),
            fact,
            MemoryType::Fact,
            importance,
        )
        .await
    }

    /**
     * Store a user preference.
     */
    pub async fn store_user_preference(
        &self,
        user_id: &str,
        preference: impl Into<String>,
        importance: f32,
    ) -> Result<MemoryEntry, MemoryStorageError> {
        let mut entry = self
            .store_memory(
                MemoryScope::User,
                None::<String>,
                Some(user_id.to_string()),
                preference,
                MemoryType::Preference,
                importance,
            )
            .await?;

        // Preferences are more important
        entry.importance_score = importance.clamp(0.7, 1.0);
        self.storage.update_memory(&entry).await?;

        Ok(entry)
    }

    /**
     * Retrieve a memory by ID.
     */
    pub async fn get_memory(&self, id: Uuid) -> Result<Option<MemoryEntry>, MemoryStorageError> {
        self.storage.get_memory(id).await
    }

    /**
     * Build context for an LLM query.
     *
     * This is the main entry point for retrieving conversational context.
     */
    pub async fn build_context(
        &self,
        query: &str,
        session_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<MemoryContext, MemoryStorageError> {
        // Get session buffer reference if provided
        // Note: We can't hold the lock across await points, so we pass None for now
        // TODO: Refactor build_memory_context to accept SessionContext instead of SessionBuffer
        let _session_ctx = if let Some(sid) = session_id {
            self.get_session_context(sid).await
        } else {
            None
        };

        // Build comprehensive context
        let context = build_memory_context(
            None, // session_buffer can't be held across await
            Arc::clone(&self.storage),
            query,
            user_id,
            &self.retriever,
        )
        .await?;

        Ok(context)
    }

    /**
     * Retrieve relevant memories for a query.
     */
    pub async fn retrieve_memories(
        &self,
        query: &str,
        user_id: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<MemoryEntry>, MemoryStorageError> {
        let limit = limit.unwrap_or(self.config.max_memories_per_query);

        let scored = self
            .retriever
            .retrieve(query, None, user_id, limit, self.config.recency_weight)
            .await?;

        Ok(scored.into_iter().map(|s| s.memory).collect())
    }

    // =========================================================================
    // Maintenance Operations
    // =========================================================================

    /**
     * Run maintenance: decay, pruning, consolidation.
     */
    pub async fn run_maintenance(&self) -> Result<MaintenanceResult, MemoryStorageError> {
        let mut result = MaintenanceResult::default();

        // 1. Clean up expired sessions
        result.expired_sessions_cleaned = self.cleanup_expired_sessions().await;

        // 2. Run storage maintenance (TTL pruning)
        let storage_stats = self.storage.run_maintenance().await?;
        result.entries_pruned += storage_stats.entries_pruned;

        // 3. Apply decay to remaining entries
        result.entries_decayed = self.apply_decay_to_memories().await?;

        // 4. Consolidate similar memories
        result.entries_consolidated = self.consolidate_memories().await?;

        info!(
            "Memory maintenance complete: {} sessions cleaned, {} entries pruned, {} decayed, {} consolidated",
            result.expired_sessions_cleaned,
            result.entries_pruned,
            result.entries_decayed,
            result.entries_consolidated
        );

        Ok(result)
    }

    /**
     * Apply decay to all memories.
     *
     * This updates the importance_score based on time since last access.
     */
    async fn apply_decay_to_memories(&self) -> Result<usize, MemoryStorageError> {
        // For efficiency, we only decay entries that might cross the threshold
        let threshold = self.config.prune_threshold * 1.5; // Some margin above prune threshold

        let entries = self
            .storage
            .get_memories_for_pruning(threshold, 1000)
            .await?;

        let mut decayed_count = 0;
        for mut entry in entries {
            let old_score = entry.importance_score;
            let new_score = calculate_decayed_importance(
                old_score,
                entry.last_accessed,
                self.config.decay_rate,
            );

            if (old_score - new_score).abs() > 0.01 {
                entry.importance_score = new_score;
                self.storage.update_memory(&entry).await?;
                decayed_count += 1;
            }
        }

        Ok(decayed_count)
    }

    /**
     * Consolidate similar memories.
     *
     * Finds memories with similar content and merges them.
     */
    async fn consolidate_memories(&self) -> Result<usize, MemoryStorageError> {
        // For now, simple consolidation: merge session summaries for same user
        // A more sophisticated approach would use semantic similarity
        // This is a placeholder for future enhancement

        Ok(0)
    }

    /**
     * Delete a memory entry.
     */
    pub async fn delete_memory(&self, id: Uuid) -> Result<bool, MemoryStorageError> {
        self.storage.delete_memory(id).await
    }

    /**
     * Get statistics about the memory system.
     */
    pub async fn get_stats(&self) -> MemoryStats {
        MemoryStats {
            active_sessions: self.session_buffers.len(),
            config: self.config.clone(),
        }
    }
}

/**
 * Result from maintenance operations.
 */
#[derive(Debug, Clone, Default)]
pub struct MaintenanceResult {
    pub expired_sessions_cleaned: usize,
    pub entries_pruned: usize,
    pub entries_decayed: usize,
    pub entries_consolidated: usize,
}

/**
 * Statistics about memory system state.
 */
#[derive(Debug, Clone)]
pub struct MemoryStats {
    pub active_sessions: usize,
    pub config: MemoryConfig,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These are mostly placeholder tests since MemoryManager requires
    // a real storage backend. Integration tests should cover full functionality.

    #[test]
    fn test_memory_config_default() {
        let config = MemoryConfig::default();
        assert!(config.enabled);
        assert!(config.decay_rate > 0.0);
        assert!(config.prune_threshold > 0.0);
        assert!(config.session_timeout_seconds > 0);
    }

    #[test]
    fn test_maintenance_result_default() {
        let result = MaintenanceResult::default();
        assert_eq!(result.expired_sessions_cleaned, 0);
        assert_eq!(result.entries_pruned, 0);
        assert_eq!(result.entries_decayed, 0);
        assert_eq!(result.entries_consolidated, 0);
    }
}
