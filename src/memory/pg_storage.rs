/**
 * PostgreSQL implementation of the MemoryStorage trait.
 *
 * Uses pgvector HNSW for embedding similarity search and standard SQL
 * for scope/user/session filtering. Schema is managed by the migration
 * in `migrations/postgres/006_memory.sql`.
 */

use async_trait::async_trait;
use pgvector::Vector;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::memory::storage::{MaintenanceStats, MemoryResult, MemoryStorage, MemoryStorageError};
use crate::memory::types::{
    MemoryEntry, MemoryMetadata, MemoryQuery, MemoryScope, MemoryType,
};

pub struct PostgresMemoryStorage {
    pool: PgPool,
}

impl PostgresMemoryStorage {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl MemoryStorage for PostgresMemoryStorage {
    async fn store_memory(&self, entry: &MemoryEntry) -> MemoryResult<Uuid> {
        let metadata_json = serde_json::to_value(&entry.metadata)
            .map_err(|e| MemoryStorageError::Database(sqlx::Error::Decode(Box::new(e))))?;
        let embedding = entry.embedding.as_ref().map(|e| Vector::from(e.clone()));

        sqlx::query(
            r#"
            INSERT INTO memory_entries (
                id, scope, session_id, user_id, content, entry_type,
                importance_score, created_at, last_accessed, access_count,
                source, ttl_seconds, metadata, embedding
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
            "#,
        )
        .bind(entry.id)
        .bind(entry.scope.as_str())
        .bind(&entry.session_id)
        .bind(&entry.user_id)
        .bind(&entry.content)
        .bind(entry.metadata.entry_type.as_str())
        .bind(entry.importance_score)
        .bind(entry.created_at)
        .bind(entry.last_accessed)
        .bind(entry.access_count as i32)
        .bind(&entry.metadata.source)
        .bind(entry.metadata.ttl_seconds)
        .bind(metadata_json)
        .bind(embedding)
        .execute(&self.pool)
        .await?;

        Ok(entry.id)
    }

    async fn get_memory(&self, id: Uuid) -> MemoryResult<Option<MemoryEntry>> {
        let row = sqlx::query(
            r#"
            SELECT id, scope, session_id, user_id, content, importance_score,
                   created_at, last_accessed, access_count, entry_type,
                   source, ttl_seconds, metadata, embedding
            FROM memory_entries WHERE id = $1 AND invalidated_at IS NULL
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => Ok(Some(pg_row_to_entry(row)?)),
            None => Ok(None),
        }
    }

    async fn update_memory(&self, entry: &MemoryEntry) -> MemoryResult<()> {
        let metadata_json = serde_json::to_value(&entry.metadata)
            .map_err(|e| MemoryStorageError::Database(sqlx::Error::Decode(Box::new(e))))?;
        let embedding = entry.embedding.as_ref().map(|e| Vector::from(e.clone()));

        sqlx::query(
            r#"
            UPDATE memory_entries SET
                content = $2, importance_score = $3, last_accessed = $4,
                access_count = $5, metadata = $6, embedding = $7
            WHERE id = $1
            "#,
        )
        .bind(entry.id)
        .bind(&entry.content)
        .bind(entry.importance_score)
        .bind(entry.last_accessed)
        .bind(entry.access_count as i32)
        .bind(metadata_json)
        .bind(embedding)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn delete_memory(&self, id: Uuid) -> MemoryResult<bool> {
        let r = sqlx::query("DELETE FROM memory_entries WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    async fn invalidate_memory(&self, id: Uuid) -> MemoryResult<bool> {
        let r = sqlx::query(
            "UPDATE memory_entries SET invalidated_at = now() WHERE id = $1 AND invalidated_at IS NULL",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    async fn query_memories(&self, query: &MemoryQuery) -> MemoryResult<Vec<MemoryEntry>> {
        if let Some(emb) = &query.embedding {
            return self.query_by_embedding(query, emb).await;
        }
        self.query_by_filters(query).await
    }

    async fn get_memories_by_scope(
        &self,
        scope: MemoryScope,
        user_id: Option<&str>,
        session_id: Option<&str>,
        limit: usize,
    ) -> MemoryResult<Vec<MemoryEntry>> {
        let rows = sqlx::query(
            r#"
            SELECT id, scope, session_id, user_id, content, importance_score,
                   created_at, last_accessed, access_count, entry_type,
                   source, ttl_seconds, metadata, embedding
            FROM memory_entries
            WHERE scope = $1 AND invalidated_at IS NULL
              AND ($2::text IS NULL OR user_id = $2)
              AND ($3::text IS NULL OR session_id = $3)
            ORDER BY last_accessed DESC
            LIMIT $4
            "#,
        )
        .bind(scope.as_str())
        .bind(user_id)
        .bind(session_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(pg_row_to_entry).collect()
    }

    async fn get_memories_for_pruning(
        &self,
        max_importance: f32,
        limit: usize,
    ) -> MemoryResult<Vec<MemoryEntry>> {
        let rows = sqlx::query(
            r#"
            SELECT id, scope, session_id, user_id, content, importance_score,
                   created_at, last_accessed, access_count, entry_type,
                   source, ttl_seconds, metadata, embedding
            FROM memory_entries
            WHERE importance_score < $1 AND invalidated_at IS NULL
            ORDER BY importance_score ASC
            LIMIT $2
            "#,
        )
        .bind(max_importance)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(pg_row_to_entry).collect()
    }

    async fn link_to_entity(&self, memory_id: Uuid, entity_id: Uuid, relationship: &str) -> MemoryResult<()> {
        sqlx::query(
            r#"
            INSERT INTO memory_entity_links (memory_id, entity_id, relationship_type)
            VALUES ($1, $2, $3)
            ON CONFLICT (memory_id, entity_id) DO UPDATE SET relationship_type = EXCLUDED.relationship_type
            "#,
        )
        .bind(memory_id)
        .bind(entity_id)
        .bind(relationship)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_memories_for_entity(&self, entity_id: Uuid, limit: usize) -> MemoryResult<Vec<MemoryEntry>> {
        let rows = sqlx::query(
            r#"
            SELECT m.id, m.scope, m.session_id, m.user_id, m.content, m.importance_score,
                   m.created_at, m.last_accessed, m.access_count, m.entry_type,
                   m.source, m.ttl_seconds, m.metadata, m.embedding
            FROM memory_entries m
            JOIN memory_entity_links mel ON m.id = mel.memory_id
            WHERE mel.entity_id = $1 AND m.invalidated_at IS NULL
            ORDER BY m.importance_score DESC
            LIMIT $2
            "#,
        )
        .bind(entity_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(pg_row_to_entry).collect()
    }

    async fn run_maintenance(&self) -> MemoryResult<MaintenanceStats> {
        let pruned = sqlx::query(
            r#"
            DELETE FROM memory_entries
            WHERE (ttl_seconds IS NOT NULL AND EXTRACT(EPOCH FROM (now() - created_at)) > ttl_seconds)
               OR (invalidated_at IS NOT NULL AND invalidated_at < now() - INTERVAL '7 days')
            "#,
        )
        .execute(&self.pool)
        .await?;

        Ok(MaintenanceStats {
            entries_pruned: pruned.rows_affected() as usize,
            entries_decayed: 0,
        })
    }

    async fn count_memories(&self) -> MemoryResult<u64> {
        let row = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM memory_entries WHERE invalidated_at IS NULL",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(row as u64)
    }

    async fn count_memories_by_scope(&self) -> MemoryResult<Vec<(String, u64)>> {
        let rows = sqlx::query(
            "SELECT scope, COUNT(*) as cnt FROM memory_entries WHERE invalidated_at IS NULL GROUP BY scope",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|r| {
                let scope: String = r.get("scope");
                let cnt: i64 = r.get("cnt");
                (scope, cnt as u64)
            })
            .collect())
    }
}

impl PostgresMemoryStorage {
    /** Vector similarity search using pgvector HNSW index. */
    async fn query_by_embedding(
        &self,
        query: &MemoryQuery,
        embedding: &[f32],
    ) -> MemoryResult<Vec<MemoryEntry>> {
        let vec = Vector::from(embedding.to_vec());
        let rows = sqlx::query(
            r#"
            SELECT id, scope, session_id, user_id, content, importance_score,
                   created_at, last_accessed, access_count, entry_type,
                   source, ttl_seconds, metadata, embedding
            FROM memory_entries
            WHERE embedding IS NOT NULL AND invalidated_at IS NULL
              AND ($1::text IS NULL OR scope = $1)
              AND ($2::text IS NULL OR user_id = $2)
              AND ($3::text IS NULL OR session_id = $3)
            ORDER BY embedding <=> $4
            LIMIT $5
            "#,
        )
        .bind(query.scope.as_ref().map(|s| s.as_str()))
        .bind(query.user_id.as_deref())
        .bind(query.session_id.as_deref())
        .bind(vec)
        .bind(query.limit as i64)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(pg_row_to_entry).collect()
    }

    /** Filter-based query without vector search. */
    async fn query_by_filters(&self, query: &MemoryQuery) -> MemoryResult<Vec<MemoryEntry>> {
        let rows = sqlx::query(
            r#"
            SELECT id, scope, session_id, user_id, content, importance_score,
                   created_at, last_accessed, access_count, entry_type,
                   source, ttl_seconds, metadata, embedding
            FROM memory_entries
            WHERE invalidated_at IS NULL
              AND ($1::text IS NULL OR scope = $1)
              AND ($2::text IS NULL OR user_id = $2)
              AND ($3::text IS NULL OR session_id = $3)
              AND ($4::text IS NULL OR entry_type = $4)
            ORDER BY importance_score DESC
            LIMIT $5
            "#,
        )
        .bind(query.scope.as_ref().map(|s| s.as_str()))
        .bind(query.user_id.as_deref())
        .bind(query.session_id.as_deref())
        .bind(query.entry_type.as_ref().map(|t| t.as_str()))
        .bind(query.limit as i64)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(pg_row_to_entry).collect()
    }
}

fn pg_row_to_entry(row: sqlx::postgres::PgRow) -> MemoryResult<MemoryEntry> {
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
        _ => MemoryType::Fact,
    };

    let metadata_json: serde_json::Value = row.try_get("metadata")?;
    let mut metadata: MemoryMetadata = serde_json::from_value(metadata_json)
        .map_err(|e| MemoryStorageError::Database(sqlx::Error::Decode(Box::new(e))))?;
    metadata.entry_type = entry_type;

    Ok(MemoryEntry {
        id: row.try_get("id")?,
        scope,
        session_id: row.try_get("session_id")?,
        user_id: row.try_get("user_id")?,
        content: row.try_get("content")?,
        importance_score: row.try_get("importance_score")?,
        created_at: row.try_get("created_at")?,
        last_accessed: row.try_get("last_accessed")?,
        access_count: row.try_get::<i32, _>("access_count")? as u32,
        embedding: row.try_get("embedding")?,
        metadata,
    })
}
