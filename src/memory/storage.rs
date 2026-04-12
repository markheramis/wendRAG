/**
 * Storage backend traits and implementations for memory persistence.
 *
 * Integrates with the existing StorageBackend system, adding memory-specific
 * operations while reusing connection pools and migration infrastructure.
 */

use crate::memory::types::{MemoryEntry, MemoryQuery, MemoryScope, MemoryType};
use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

/**
 * Error type for memory storage operations.
 */
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

/**
 * Result type for memory storage operations.
 */
pub type MemoryResult<T> = Result<T, MemoryStorageError>;

/**
 * Trait for memory storage backends.
 *
 * Implementations can use PostgreSQL, SQLite, or other storage systems.
 */
#[async_trait]
pub trait MemoryStorage: Send + Sync {
    /**
     * Store a new memory entry.
     */
    async fn store_memory(&self, entry: &MemoryEntry) -> MemoryResult<Uuid>;

    /**
     * Retrieve a memory entry by ID.
     */
    async fn get_memory(&self, id: Uuid) -> MemoryResult<Option<MemoryEntry>>;

    /**
     * Update an existing memory entry.
     */
    async fn update_memory(&self, entry: &MemoryEntry) -> MemoryResult<()>;

    /**
     * Delete a memory entry.
     */
    async fn delete_memory(&self, id: Uuid) -> MemoryResult<bool>;

    /**
     * Query memories with filters.
     *
     * For semantic search, implementations should use the query_embedding if provided,
     * otherwise fall back to text search or metadata filtering.
     */
    async fn query_memories(&self, query: &MemoryQuery) -> MemoryResult<Vec<MemoryEntry>>;

    /**
     * Get memories by scope and user/session IDs.
     */
    async fn get_memories_by_scope(
        &self,
        scope: MemoryScope,
        user_id: Option<&str>,
        session_id: Option<&str>,
        limit: usize,
    ) -> MemoryResult<Vec<MemoryEntry>>;

    /**
     * Get memories below importance threshold for pruning.
     */
    async fn get_memories_for_pruning(
        &self,
        min_importance: f32,
        limit: usize,
    ) -> MemoryResult<Vec<MemoryEntry>>;

    /**
     * Link a memory entry to an entity.
     */
    async fn link_to_entity(
        &self,
        memory_id: Uuid,
        entity_id: Uuid,
        relationship: &str,
    ) -> MemoryResult<()>;

    /**
     * Get memories linked to an entity.
     */
    async fn get_memories_for_entity(
        &self,
        entity_id: Uuid,
        limit: usize,
    ) -> MemoryResult<Vec<MemoryEntry>>;

    /**
     * Run maintenance: delete expired entries, update decay scores.
     */
    async fn run_maintenance(&self) -> MemoryResult<MaintenanceStats>;
}

/**
 * Statistics from maintenance operations.
 */
#[derive(Debug, Clone)]
pub struct MaintenanceStats {
    pub entries_pruned: usize,
    pub entries_consolidated: usize,
    pub entries_decayed: usize,
}

/**
 * PostgreSQL implementation of memory storage.
 */
pub struct PostgresMemoryStorage {
    pool: PgPool,
}

impl PostgresMemoryStorage {
    /**
     * Create a new PostgreSQL memory storage instance.
     */
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /**
     * Initialize database schema for memory storage.
     *
     * This should be called as part of migrations.
     */
    pub async fn initialize_schema(&self) -> MemoryResult<()> {
        // Memory entries table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS memory_entries (
                id UUID PRIMARY KEY,
                scope VARCHAR(20) NOT NULL,
                session_id VARCHAR(255),
                user_id VARCHAR(255),
                content TEXT NOT NULL,
                importance_score FLOAT NOT NULL DEFAULT 0.5,
                created_at TIMESTAMP NOT NULL DEFAULT NOW(),
                last_accessed TIMESTAMP NOT NULL DEFAULT NOW(),
                access_count INTEGER NOT NULL DEFAULT 0,
                entry_type VARCHAR(50) NOT NULL,
                source VARCHAR(255) NOT NULL DEFAULT 'memory_system',
                ttl_seconds INTEGER,
                consolidation_target UUID REFERENCES memory_entries(id) ON DELETE SET NULL,
                metadata JSONB DEFAULT '{}',
                embedding VECTOR(1536)
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Indexes
        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_memory_session 
            ON memory_entries(session_id, created_at DESC)
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_memory_user 
            ON memory_entries(user_id, importance_score DESC)
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_memory_scope_type 
            ON memory_entries(scope, entry_type)
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_memory_accessed 
            ON memory_entries(last_accessed DESC)
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Memory-entity links table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS memory_entity_links (
                memory_id UUID REFERENCES memory_entries(id) ON DELETE CASCADE,
                entity_id UUID REFERENCES entities(id) ON DELETE CASCADE,
                relationship_type VARCHAR(50),
                PRIMARY KEY (memory_id, entity_id)
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_memory_entity 
            ON memory_entity_links(entity_id)
            "#,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}

#[async_trait]
impl MemoryStorage for PostgresMemoryStorage {
    async fn store_memory(&self, entry: &MemoryEntry) -> MemoryResult<Uuid> {
        let metadata_json = serde_json::to_value(&entry.metadata)
            .map_err(|e| MemoryStorageError::Database(sqlx::Error::Decode(Box::new(e))))?;

        sqlx::query(
            r#"
            INSERT INTO memory_entries (
                id, scope, session_id, user_id, content, importance_score,
                created_at, last_accessed, access_count, entry_type,
                source, ttl_seconds, consolidation_target, metadata, embedding
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
            "#,
        )
        .bind(entry.id)
        .bind(entry.scope.as_str())
        .bind(&entry.session_id)
        .bind(&entry.user_id)
        .bind(&entry.content)
        .bind(entry.importance_score)
        .bind(entry.created_at)
        .bind(entry.last_accessed)
        .bind(entry.access_count as i32)
        .bind(entry.metadata.entry_type.as_str())
        .bind(&entry.metadata.source)
        .bind(entry.metadata.ttl_seconds)
        .bind(entry.metadata.consolidation_target)
        .bind(metadata_json)
        .bind(entry.embedding.as_ref().map(|e| e.as_slice()))
        .execute(&self.pool)
        .await?;

        Ok(entry.id)
    }

    async fn get_memory(&self, id: Uuid) -> MemoryResult<Option<MemoryEntry>> {
        let row = sqlx::query(
            r#"
            SELECT id, scope, session_id, user_id, content, importance_score,
                   created_at, last_accessed, access_count, entry_type,
                   source, ttl_seconds, consolidation_target, metadata, embedding
            FROM memory_entries
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => Ok(Some(row_to_memory_entry(row)?)),
            None => Ok(None),
        }
    }

    async fn update_memory(&self, entry: &MemoryEntry) -> MemoryResult<()> {
        let metadata_json = serde_json::to_value(&entry.metadata)
            .map_err(|e| MemoryStorageError::Database(sqlx::Error::Decode(Box::new(e))))?;

        sqlx::query(
            r#"
            UPDATE memory_entries
            SET scope = $2,
                session_id = $3,
                user_id = $4,
                content = $5,
                importance_score = $6,
                last_accessed = $7,
                access_count = $8,
                entry_type = $9,
                source = $10,
                ttl_seconds = $11,
                consolidation_target = $12,
                metadata = $13,
                embedding = $14
            WHERE id = $1
            "#,
        )
        .bind(entry.id)
        .bind(entry.scope.as_str())
        .bind(&entry.session_id)
        .bind(&entry.user_id)
        .bind(&entry.content)
        .bind(entry.importance_score)
        .bind(entry.last_accessed)
        .bind(entry.access_count as i32)
        .bind(entry.metadata.entry_type.as_str())
        .bind(&entry.metadata.source)
        .bind(entry.metadata.ttl_seconds)
        .bind(entry.metadata.consolidation_target)
        .bind(metadata_json)
        .bind(entry.embedding.as_ref().map(|e| e.as_slice()))
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn delete_memory(&self, id: Uuid) -> MemoryResult<bool> {
        let result = sqlx::query("DELETE FROM memory_entries WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;

        Ok(result.rows_affected() > 0)
    }

    async fn query_memories(&self, query: &MemoryQuery) -> MemoryResult<Vec<MemoryEntry>> {
        let mut sql = String::from(
            "SELECT id, scope, session_id, user_id, content, importance_score, \
             created_at, last_accessed, access_count, entry_type, \
             source, ttl_seconds, consolidation_target, metadata, embedding \
             FROM memory_entries WHERE 1=1"
        );

        if query.scope.is_some() {
            sql.push_str(" AND scope = $1");
        }
        if query.session_id.is_some() {
            sql.push_str(&format!(" AND session_id = ${}", query.scope.is_some() as usize + 1));
        }
        if query.user_id.is_some() {
            sql.push_str(&format!(" AND user_id = ${}", 
                query.scope.is_some() as usize + query.session_id.is_some() as usize + 1));
        }

        sql.push_str(&format!(" ORDER BY importance_score DESC LIMIT ${}", 
            query.scope.is_some() as usize + query.session_id.is_some() as usize + 
            query.user_id.is_some() as usize + 1));

        let mut query_builder = sqlx::query(&sql);

        if let Some(scope) = &query.scope {
            query_builder = query_builder.bind(scope.as_str());
        }
        if let Some(session_id) = &query.session_id {
            query_builder = query_builder.bind(session_id);
        }
        if let Some(user_id) = &query.user_id {
            query_builder = query_builder.bind(user_id);
        }
        query_builder = query_builder.bind(query.limit as i64);

        let rows = query_builder.fetch_all(&self.pool).await?;
        
        rows.into_iter()
            .map(|row| row_to_memory_entry(row))
            .collect::<Result<Vec<_>, _>>()
    }

    async fn get_memories_by_scope(
        &self,
        scope: MemoryScope,
        user_id: Option<&str>,
        session_id: Option<&str>,
        limit: usize,
    ) -> MemoryResult<Vec<MemoryEntry>> {
        let query = MemoryQuery::new()
            .scope(scope)
            .limit(limit);
        
        self.query_memories(&query).await
    }

    async fn get_memories_for_pruning(
        &self,
        min_importance: f32,
        limit: usize,
    ) -> MemoryResult<Vec<MemoryEntry>> {
        let rows = sqlx::query(
            r#"
            SELECT id, scope, session_id, user_id, content, importance_score,
                   created_at, last_accessed, access_count, entry_type,
                   source, ttl_seconds, consolidation_target, metadata, embedding
            FROM memory_entries
            WHERE importance_score < $1
            ORDER BY importance_score ASC
            LIMIT $2
            "#,
        )
        .bind(min_importance)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(row_to_memory_entry)
            .collect()
    }

    async fn link_to_entity(
        &self,
        memory_id: Uuid,
        entity_id: Uuid,
        relationship: &str,
    ) -> MemoryResult<()> {
        sqlx::query(
            r#"
            INSERT INTO memory_entity_links (memory_id, entity_id, relationship_type)
            VALUES ($1, $2, $3)
            ON CONFLICT (memory_id, entity_id) DO UPDATE SET
                relationship_type = EXCLUDED.relationship_type
            "#,
        )
        .bind(memory_id)
        .bind(entity_id)
        .bind(relationship)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn get_memories_for_entity(
        &self,
        entity_id: Uuid,
        limit: usize,
    ) -> MemoryResult<Vec<MemoryEntry>> {
        let rows = sqlx::query(
            r#"
            SELECT m.id, m.scope, m.session_id, m.user_id, m.content, m.importance_score,
                   m.created_at, m.last_accessed, m.access_count, m.entry_type,
                   m.source, m.ttl_seconds, m.consolidation_target, m.metadata, m.embedding
            FROM memory_entries m
            JOIN memory_entity_links mel ON m.id = mel.memory_id
            WHERE mel.entity_id = $1
            ORDER BY m.importance_score DESC
            LIMIT $2
            "#,
        )
        .bind(entity_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(row_to_memory_entry)
            .collect()
    }

    async fn run_maintenance(&self) -> MemoryResult<MaintenanceStats> {
        // Delete expired entries
        let pruned = sqlx::query(
            r#"
            DELETE FROM memory_entries
            WHERE ttl_seconds IS NOT NULL
            AND EXTRACT(EPOCH FROM (NOW() - created_at)) > ttl_seconds
            "#,
        )
        .execute(&self.pool)
        .await?;

        let stats = MaintenanceStats {
            entries_pruned: pruned.rows_affected() as usize,
            entries_consolidated: 0,
            entries_decayed: 0,
        };

        Ok(stats)
    }
}

/**
 * Convert a database row to a MemoryEntry.
 */
fn row_to_memory_entry(row: sqlx::postgres::PgRow) -> MemoryResult<MemoryEntry> {
    use sqlx::Row;
    
    let id: Uuid = row.try_get("id")?;
    let scope_str: String = row.try_get("scope")?;
    let scope = match scope_str.as_str() {
        "session" => MemoryScope::Session,
        "user" => MemoryScope::User,
        "global" => MemoryScope::Global,
        _ => return Err(MemoryStorageError::InvalidScope(scope_str)),
    };

    let entry_type_str: String = row.try_get("entry_type")?;
    let entry_type = match entry_type_str.as_str() {
        "fact" => MemoryType::Fact,
        "preference" => MemoryType::Preference,
        "event" => MemoryType::Event,
        "summary" => MemoryType::Summary,
        "message" => MemoryType::Message,
        _ => MemoryType::Fact, // Default
    };

    let metadata_json: serde_json::Value = row.try_get("metadata")?;
    let metadata: crate::memory::types::MemoryMetadata = 
        serde_json::from_value(metadata_json)
            .map_err(|e| MemoryStorageError::Database(sqlx::Error::Decode(Box::new(e))))?;

    Ok(MemoryEntry {
        id,
        scope,
        session_id: row.try_get("session_id")?,
        user_id: row.try_get("user_id")?,
        content: row.try_get("content")?,
        importance_score: row.try_get("importance_score")?,
        created_at: row.try_get::<chrono::DateTime<chrono::Utc>, _>("created_at")?,
        last_accessed: row.try_get::<chrono::DateTime<chrono::Utc>, _>("last_accessed")?,
        access_count: row.try_get::<i32, _>("access_count")? as u32,
        embedding: row.try_get("embedding")?,
        metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::types::{MemoryEntry, MemoryScope, MemoryType};

    // Note: These tests require a database connection
    // They are marked as integration tests

    #[test]
    fn test_memory_scope_as_str() {
        assert_eq!(MemoryScope::Session.as_str(), "session");
        assert_eq!(MemoryScope::User.as_str(), "user");
        assert_eq!(MemoryScope::Global.as_str(), "global");
    }

    #[test]
    fn test_memory_type_as_str() {
        assert_eq!(MemoryType::Fact.as_str(), "fact");
        assert_eq!(MemoryType::Preference.as_str(), "preference");
        assert_eq!(MemoryType::Event.as_str(), "event");
        assert_eq!(MemoryType::Summary.as_str(), "summary");
        assert_eq!(MemoryType::Message.as_str(), "message");
    }
}
