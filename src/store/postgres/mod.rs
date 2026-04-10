/**
 * PostgreSQL storage backend: struct definition, connection, document CRUD,
 * and trait implementation that delegates search and entity graph operations
 * to focused submodules.
 */

mod entity_graph;
mod search;

use async_trait::async_trait;
use chrono::Utc;
use pgvector::Vector;
use sqlx::PgPool;
use uuid::Uuid;

use crate::entity::DocumentEntityGraph;
use crate::retrieve::ScoredChunk;
use crate::store::models::{Document, DocumentChunk, DocumentWithChunkCount};

use crate::config::PoolConfig;

use super::{ChunkInsert, DocumentUpsert, POSTGRES_MIGRATOR, SearchFilters, StorageBackend};

#[derive(Debug, Clone)]
pub struct PostgresBackend {
    pool: PgPool,
}

impl PostgresBackend {
    /**
     * Connects to PostgreSQL, runs the pgvector migrations, and returns the
     * backend handle shared by the rest of the application. Pool size and
     * acquire timeout are driven by the caller-provided [`PoolConfig`].
     */
    pub async fn connect(database_url: &str, pool_cfg: &PoolConfig) -> Result<Self, sqlx::Error> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(pool_cfg.max_connections)
            .acquire_timeout(pool_cfg.acquire_timeout)
            .connect(database_url)
            .await?;
        POSTGRES_MIGRATOR.run(&pool).await?;
        Ok(Self { pool })
    }
}

#[async_trait]
impl StorageBackend for PostgresBackend {
    async fn get_document_by_path(&self, file_path: &str) -> Result<Option<Document>, sqlx::Error> {
        sqlx::query_as::<_, Document>(
            "SELECT id, content_hash, created_at, updated_at FROM documents WHERE file_path = $1",
        )
        .bind(file_path)
        .fetch_optional(&self.pool)
        .await
    }

    async fn upsert_document(&self, input: &DocumentUpsert) -> Result<Uuid, sqlx::Error> {
        let now = Utc::now();
        let row: (Uuid,) = sqlx::query_as(
            r#"
            INSERT INTO documents (file_path, file_name, file_type, content_hash, project, tags, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $7)
            ON CONFLICT (file_path) DO UPDATE SET
                file_name = EXCLUDED.file_name,
                content_hash = EXCLUDED.content_hash,
                project = EXCLUDED.project,
                tags = EXCLUDED.tags,
                updated_at = EXCLUDED.updated_at
            RETURNING id
            "#,
        )
        .bind(&input.file_path)
        .bind(&input.file_name)
        .bind(&input.file_type)
        .bind(&input.content_hash)
        .bind(input.project.as_deref())
        .bind(&input.tags)
        .bind(now)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.0)
    }

    async fn replace_document_chunks(
        &self,
        document_id: Uuid,
        chunks: &[ChunkInsert],
    ) -> Result<(), sqlx::Error> {
        /// Rows per multi-row INSERT batch. 5 bind params per row; Postgres
        /// supports up to 65 535 params, so 50 rows (250 params) is safe.
        const CHUNK_BATCH_SIZE: usize = 50;

        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
            DELETE FROM entity_relationships
            WHERE evidence_chunk_id IN (
                SELECT id
                FROM chunks
                WHERE document_id = $1
            )
            "#,
        )
        .bind(document_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM chunks WHERE document_id = $1")
            .bind(document_id)
            .execute(&mut *tx)
            .await?;

        for batch in chunks.chunks(CHUNK_BATCH_SIZE) {
            let mut builder = sqlx::QueryBuilder::new(
                "INSERT INTO chunks (document_id, content, chunk_index, section_title, embedding) ",
            );
            builder.push_values(batch, |mut row, chunk| {
                row.push_bind(document_id)
                    .push_bind(&chunk.content)
                    .push_bind(chunk.chunk_index)
                    .push_bind(chunk.section_title.as_deref())
                    .push_bind(Vector::from(chunk.embedding.clone()));
            });
            builder.build().execute(&mut *tx).await?;
        }

        tx.commit().await
    }

    async fn replace_document_entity_graph(
        &self,
        document_id: Uuid,
        graph: &DocumentEntityGraph,
    ) -> Result<(), sqlx::Error> {
        entity_graph::replace_document_entity_graph(&self.pool, document_id, graph).await
    }

    async fn search_dense(
        &self,
        query_embedding: &[f32],
        top_k: i64,
        filters: &SearchFilters,
    ) -> Result<Vec<ScoredChunk>, sqlx::Error> {
        search::search_dense(&self.pool, query_embedding, top_k, filters).await
    }

    async fn search_sparse(
        &self,
        query: &str,
        top_k: i64,
        filters: &SearchFilters,
    ) -> Result<Vec<ScoredChunk>, sqlx::Error> {
        search::search_sparse(&self.pool, query, top_k, filters).await
    }

    async fn search_graph(
        &self,
        seed_chunk_ids: &[Uuid],
        top_k: i64,
        filters: &SearchFilters,
        traversal_depth: u8,
    ) -> Result<Vec<ScoredChunk>, sqlx::Error> {
        search::search_graph(&self.pool, seed_chunk_ids, top_k, filters, traversal_depth).await
    }

    async fn list_documents(
        &self,
        project: Option<&str>,
        file_type: Option<&str>,
    ) -> Result<Vec<DocumentWithChunkCount>, sqlx::Error> {
        sqlx::query_as::<_, DocumentWithChunkCount>(
            r#"
            SELECT
                d.id, d.file_path, d.file_name, d.file_type, d.project, d.tags,
                d.created_at, d.updated_at,
                COUNT(c.id) AS chunk_count
            FROM documents d
            LEFT JOIN chunks c ON c.document_id = d.id
            WHERE ($1::text IS NULL OR d.project = $1)
              AND ($2::text IS NULL OR d.file_type = $2)
            GROUP BY d.id
            ORDER BY d.updated_at DESC
            "#,
        )
        .bind(project)
        .bind(file_type)
        .fetch_all(&self.pool)
        .await
    }

    async fn get_document_chunks(
        &self,
        file_path: &str,
    ) -> Result<Vec<DocumentChunk>, sqlx::Error> {
        sqlx::query_as::<_, DocumentChunk>(
            r#"
            SELECT
                c.content,
                c.chunk_index,
                c.section_title
            FROM chunks c
            JOIN documents d ON d.id = c.document_id
            WHERE d.file_path = $1
            ORDER BY c.chunk_index ASC
            "#,
        )
        .bind(file_path)
        .fetch_all(&self.pool)
        .await
    }

    async fn delete_document(
        &self,
        file_path: Option<&str>,
        document_id: Option<Uuid>,
    ) -> Result<Option<(String, i64)>, sqlx::Error> {
        let document = if let Some(id) = document_id {
            sqlx::query_as::<_, (String,)>("SELECT file_path FROM documents WHERE id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?
        } else if let Some(path) = file_path {
            sqlx::query_as::<_, (String,)>("SELECT file_path FROM documents WHERE file_path = $1")
                .bind(path)
                .fetch_optional(&self.pool)
                .await?
        } else {
            return Ok(None);
        };

        let Some((path,)) = document else {
            return Ok(None);
        };

        let chunk_count = sqlx::query_as::<_, (i64,)>(
            "SELECT COUNT(*) FROM chunks c JOIN documents d ON c.document_id = d.id WHERE d.file_path = $1",
        )
        .bind(&path)
        .fetch_one(&self.pool)
        .await?
        .0;

        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
            DELETE FROM entity_relationships
            WHERE evidence_chunk_id IN (
                SELECT chunk.id
                FROM chunks AS chunk
                JOIN documents AS document
                    ON document.id = chunk.document_id
                WHERE document.file_path = $1
            )
            "#,
        )
        .bind(&path)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM documents WHERE file_path = $1")
            .bind(&path)
            .execute(&mut *tx)
            .await?;
        entity_graph::prune_orphan_entities(&mut tx).await?;
        tx.commit().await?;

        Ok(Some((path, chunk_count)))
    }
}
