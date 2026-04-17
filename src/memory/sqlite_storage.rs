/*!
 * SQLite implementation of the MemoryStorage trait.
 *
 * Stores embeddings as BLOBs and computes cosine similarity in Rust
 * for semantic search. Mirrors the pattern in `store/sqlite/community.rs`.
 */

use async_trait::async_trait;
use chrono::Utc;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::memory::storage::{MaintenanceStats, MemoryResult, MemoryStorage, MemoryStorageError};
use crate::memory::types::{
    MemoryEntry, MemoryMetadata, MemoryQuery, MemoryScope, MemoryType,
};
use crate::store::sqlite::embeddings::{
    cosine_similarity, decode_embedding_blob, embedding_to_blob,
};

pub struct SqliteMemoryStorage {
    pool: SqlitePool,
}

impl SqliteMemoryStorage {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl MemoryStorage for SqliteMemoryStorage {
    async fn store_memory(&self, entry: &MemoryEntry) -> MemoryResult<Uuid> {
        let metadata_json = serde_json::to_string(&entry.metadata)
            .map_err(|e| MemoryStorageError::Database(sqlx::Error::Decode(Box::new(e))))?;
        let emb_blob = entry.embedding.as_ref().map(|e| embedding_to_blob(e));
        let now = entry.created_at.to_rfc3339();
        let last = entry.last_accessed.to_rfc3339();

        sqlx::query(
            r#"
            INSERT INTO memory_entries (
                id, scope, session_id, user_id, content, entry_type,
                importance_score, created_at, last_accessed, access_count,
                source, ttl_seconds, metadata, embedding
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(entry.id.to_string())
        .bind(entry.scope.as_str())
        .bind(&entry.session_id)
        .bind(&entry.user_id)
        .bind(&entry.content)
        .bind(entry.metadata.entry_type.as_str())
        .bind(entry.importance_score)
        .bind(&now)
        .bind(&last)
        .bind(entry.access_count as i32)
        .bind(&entry.metadata.source)
        .bind(entry.metadata.ttl_seconds)
        .bind(metadata_json)
        .bind(emb_blob)
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
            FROM memory_entries WHERE id = ? AND invalidated_at IS NULL
            "#,
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => Ok(Some(sqlite_row_to_entry(&row)?)),
            None => Ok(None),
        }
    }

    async fn update_memory(&self, entry: &MemoryEntry) -> MemoryResult<()> {
        let metadata_json = serde_json::to_string(&entry.metadata)
            .map_err(|e| MemoryStorageError::Database(sqlx::Error::Decode(Box::new(e))))?;
        let emb_blob = entry.embedding.as_ref().map(|e| embedding_to_blob(e));
        let last = entry.last_accessed.to_rfc3339();

        sqlx::query(
            r#"
            UPDATE memory_entries SET
                content = ?, importance_score = ?, last_accessed = ?,
                access_count = ?, metadata = ?, embedding = ?
            WHERE id = ?
            "#,
        )
        .bind(&entry.content)
        .bind(entry.importance_score)
        .bind(&last)
        .bind(entry.access_count as i32)
        .bind(metadata_json)
        .bind(emb_blob)
        .bind(entry.id.to_string())
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn delete_memory(&self, id: Uuid) -> MemoryResult<bool> {
        let r = sqlx::query("DELETE FROM memory_entries WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    async fn invalidate_memory(&self, id: Uuid) -> MemoryResult<bool> {
        let now = Utc::now().to_rfc3339();
        let r = sqlx::query(
            "UPDATE memory_entries SET invalidated_at = ? WHERE id = ? AND invalidated_at IS NULL",
        )
        .bind(&now)
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    async fn query_memories(&self, query: &MemoryQuery) -> MemoryResult<Vec<MemoryEntry>> {
        let rows = sqlx::query(
            r#"
            SELECT id, scope, session_id, user_id, content, importance_score,
                   created_at, last_accessed, access_count, entry_type,
                   source, ttl_seconds, metadata, embedding
            FROM memory_entries
            WHERE invalidated_at IS NULL
              AND (? IS NULL OR scope = ?)
              AND (? IS NULL OR user_id = ?)
              AND (? IS NULL OR session_id = ?)
              AND (? IS NULL OR entry_type = ?)
            ORDER BY importance_score DESC
            LIMIT ?
            "#,
        )
        .bind(query.scope.as_ref().map(|s| s.as_str()))
        .bind(query.scope.as_ref().map(|s| s.as_str()))
        .bind(query.user_id.as_deref())
        .bind(query.user_id.as_deref())
        .bind(query.session_id.as_deref())
        .bind(query.session_id.as_deref())
        .bind(query.entry_type.as_ref().map(|t| t.as_str()))
        .bind(query.entry_type.as_ref().map(|t| t.as_str()))
        .bind(query.limit as i64)
        .fetch_all(&self.pool)
        .await?;

        if let Some(query_emb) = &query.embedding {
            return self.rerank_by_embedding(&rows, query_emb, query.limit);
        }

        rows.iter().map(sqlite_row_to_entry).collect()
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
            WHERE scope = ? AND invalidated_at IS NULL
              AND (? IS NULL OR user_id = ?)
              AND (? IS NULL OR session_id = ?)
            ORDER BY last_accessed DESC
            LIMIT ?
            "#,
        )
        .bind(scope.as_str())
        .bind(user_id)
        .bind(user_id)
        .bind(session_id)
        .bind(session_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(sqlite_row_to_entry).collect()
    }

    async fn get_memories_for_pruning(&self, max_importance: f32, limit: usize) -> MemoryResult<Vec<MemoryEntry>> {
        let rows = sqlx::query(
            r#"
            SELECT id, scope, session_id, user_id, content, importance_score,
                   created_at, last_accessed, access_count, entry_type,
                   source, ttl_seconds, metadata, embedding
            FROM memory_entries
            WHERE importance_score < ? AND invalidated_at IS NULL
            ORDER BY importance_score ASC LIMIT ?
            "#,
        )
        .bind(max_importance)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(sqlite_row_to_entry).collect()
    }

    async fn link_to_entity(&self, memory_id: Uuid, entity_id: Uuid, relationship: &str) -> MemoryResult<()> {
        sqlx::query(
            r#"
            INSERT INTO memory_entity_links (memory_id, entity_id, relationship_type)
            VALUES (?, ?, ?) ON CONFLICT (memory_id, entity_id) DO UPDATE
            SET relationship_type = excluded.relationship_type
            "#,
        )
        .bind(memory_id.to_string())
        .bind(entity_id.to_string())
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
            WHERE mel.entity_id = ? AND m.invalidated_at IS NULL
            ORDER BY m.importance_score DESC LIMIT ?
            "#,
        )
        .bind(entity_id.to_string())
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(sqlite_row_to_entry).collect()
    }

    async fn run_maintenance(&self) -> MemoryResult<MaintenanceStats> {
        let now_epoch = Utc::now().timestamp();
        let pruned = sqlx::query(
            "DELETE FROM memory_entries WHERE ttl_seconds IS NOT NULL AND (? - CAST(strftime('%s', created_at) AS INTEGER)) > ttl_seconds",
        )
        .bind(now_epoch)
        .execute(&self.pool)
        .await?;

        let week_ago = (Utc::now() - chrono::Duration::days(7)).to_rfc3339();
        let invalidated = sqlx::query(
            "DELETE FROM memory_entries WHERE invalidated_at IS NOT NULL AND invalidated_at < ?",
        )
        .bind(&week_ago)
        .execute(&self.pool)
        .await?;

        Ok(MaintenanceStats {
            entries_pruned: (pruned.rows_affected() + invalidated.rows_affected()) as usize,
            entries_decayed: 0,
        })
    }

    async fn count_memories(&self) -> MemoryResult<u64> {
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM memory_entries WHERE invalidated_at IS NULL",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(count as u64)
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

impl SqliteMemoryStorage {
    /** Reranks fetched rows by cosine similarity to the query embedding. */
    fn rerank_by_embedding(
        &self,
        rows: &[sqlx::sqlite::SqliteRow],
        query_emb: &[f32],
        limit: usize,
    ) -> MemoryResult<Vec<MemoryEntry>> {
        let mut scored: Vec<(f64, MemoryEntry)> = rows
            .iter()
            .filter_map(|row| {
                let entry = sqlite_row_to_entry(row).ok()?;
                let blob: Option<Vec<u8>> = row.get("embedding");
                let sim = blob
                    .and_then(|b| decode_embedding_blob(&b).ok())
                    .and_then(|emb| cosine_similarity(query_emb, &emb).ok())
                    .unwrap_or(0.0);
                Some((sim, entry))
            })
            .collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored.into_iter().map(|(_, e)| e).collect())
    }
}

fn sqlite_row_to_entry(row: &sqlx::sqlite::SqliteRow) -> MemoryResult<MemoryEntry> {
    let id_str: String = row.get("id");
    let scope_str: String = row.get("scope");
    let scope = match scope_str.as_str() {
        "session" => MemoryScope::Session,
        "user" => MemoryScope::User,
        "global" => MemoryScope::Global,
        _ => return Err(MemoryStorageError::InvalidScope(scope_str)),
    };

    let entry_type_str: String = row.get("entry_type");
    let entry_type = match entry_type_str.as_str() {
        "fact" => MemoryType::Fact,
        "preference" => MemoryType::Preference,
        "event" => MemoryType::Event,
        "summary" => MemoryType::Summary,
        "message" => MemoryType::Message,
        _ => MemoryType::Fact,
    };

    let metadata_str: String = row.get("metadata");
    let mut metadata: MemoryMetadata = serde_json::from_str(&metadata_str)
        .map_err(|e| MemoryStorageError::Database(sqlx::Error::Decode(Box::new(e))))?;
    metadata.entry_type = entry_type;

    let created_str: String = row.get("created_at");
    let accessed_str: String = row.get("last_accessed");

    Ok(MemoryEntry {
        id: Uuid::parse_str(&id_str).unwrap_or_default(),
        scope,
        session_id: row.get("session_id"),
        user_id: row.get("user_id"),
        content: row.get("content"),
        importance_score: row.get("importance_score"),
        created_at: chrono::DateTime::parse_from_rfc3339(&created_str)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| Utc::now()),
        last_accessed: chrono::DateTime::parse_from_rfc3339(&accessed_str)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| Utc::now()),
        access_count: row.get::<i32, _>("access_count") as u32,
        embedding: row
            .get::<Option<Vec<u8>>, _>("embedding")
            .and_then(|b| decode_embedding_blob(&b).ok()),
        metadata,
    })
}
