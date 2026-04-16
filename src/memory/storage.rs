/**
 * Storage trait for the memory persistence layer.
 *
 * Backends implement this trait against PostgreSQL or SQLite. The trait
 * intentionally uses `dyn`-safe signatures so callers can hold
 * `Arc<dyn MemoryStorage>` without generics propagating up the stack.
 */

use async_trait::async_trait;
use uuid::Uuid;

use crate::memory::types::{MemoryEntry, MemoryQuery, MemoryScope};

#[derive(Debug, thiserror::Error)]
pub enum MemoryStorageError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("entry not found: {0}")]
    NotFound(Uuid),
    #[error("invalid scope: {0}")]
    InvalidScope(String),
    #[error("embedding error: {0}")]
    Embedding(String),
}

pub type MemoryResult<T> = Result<T, MemoryStorageError>;

#[derive(Debug, Clone, Default)]
pub struct MaintenanceStats {
    pub entries_pruned: usize,
    pub entries_decayed: usize,
}

#[async_trait]
pub trait MemoryStorage: Send + Sync {
    async fn store_memory(&self, entry: &MemoryEntry) -> MemoryResult<Uuid>;
    async fn get_memory(&self, id: Uuid) -> MemoryResult<Option<MemoryEntry>>;
    async fn update_memory(&self, entry: &MemoryEntry) -> MemoryResult<()>;
    async fn delete_memory(&self, id: Uuid) -> MemoryResult<bool>;

    /** Soft-delete: sets `invalidated_at` instead of removing the row. */
    async fn invalidate_memory(&self, id: Uuid) -> MemoryResult<bool>;

    /**
     * Queries memories with filters and optional vector similarity search.
     * When `query.embedding` is set, results are ranked by cosine distance.
     */
    async fn query_memories(&self, query: &MemoryQuery) -> MemoryResult<Vec<MemoryEntry>>;

    async fn get_memories_by_scope(
        &self,
        scope: MemoryScope,
        user_id: Option<&str>,
        session_id: Option<&str>,
        limit: usize,
    ) -> MemoryResult<Vec<MemoryEntry>>;

    async fn get_memories_for_pruning(
        &self,
        max_importance: f32,
        limit: usize,
    ) -> MemoryResult<Vec<MemoryEntry>>;

    async fn link_to_entity(
        &self,
        memory_id: Uuid,
        entity_id: Uuid,
        relationship: &str,
    ) -> MemoryResult<()>;

    async fn get_memories_for_entity(
        &self,
        entity_id: Uuid,
        limit: usize,
    ) -> MemoryResult<Vec<MemoryEntry>>;

    /** TTL pruning and expired-entry cleanup. */
    async fn run_maintenance(&self) -> MemoryResult<MaintenanceStats>;

    /** Total count of non-invalidated memory entries for status reporting. */
    async fn count_memories(&self) -> MemoryResult<u64>;

    /** Count memories grouped by scope for the status resource. */
    async fn count_memories_by_scope(&self) -> MemoryResult<Vec<(String, u64)>>;
}
