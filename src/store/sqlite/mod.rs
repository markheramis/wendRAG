/*!
 * SQLite storage backend: struct definition, connection, and trait
 * implementation that delegates to focused submodules.
 */

mod community;
pub(crate) mod embeddings;
mod entity_graph;
mod filters;
mod mappers;
mod text_util;

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::Utc;
use sqlx::Row;
use sqlx::sqlite::Sqlite;
use sqlx::{QueryBuilder, SqlitePool};
use uuid::Uuid;

use crate::entity::{CommunityWithSummary, DocumentEntityGraph};
use crate::retrieve::ScoredChunk;
use crate::store::models::{
    Document, DocumentChunk, DocumentChunkWithMeta, DocumentWithChunkCount, StoredCommunity,
};

use crate::config::PoolConfig;

use super::{
    ChunkInsert, DocumentUpsert, SQLITE_MIGRATOR, SearchFilters, StorageBackend,
    connect_sqlite_pool,
};

use embeddings::{
    cosine_similarity, decode_embedding_blob, embedding_to_blob, validate_embedding_dimensions,
};
use entity_graph::prune_orphan_entities;
use filters::push_sqlite_document_filters;
use mappers::{
    encode_tags, map_document_chunk_row, map_document_chunk_with_meta_row, map_document_row,
    map_document_with_chunk_count_row,
    map_scored_chunk_row, parse_uuid_text,
};
use text_util::{build_fts5_query, escape_like_pattern, trigram_similarity};

#[derive(Debug, Clone)]
pub struct SqliteBackend {
    pool: SqlitePool,
}

impl SqliteBackend {
    /**
     * Connects to the SQLite database file and applies the SQLite-specific
     * migrations. Pool size and acquire timeout are driven by the
     * caller-provided [`PoolConfig`].
     */
    pub async fn connect(sqlite_path: &str, pool_cfg: &PoolConfig) -> Result<Self, sqlx::Error> {
        let pool = connect_sqlite_pool(sqlite_path, pool_cfg)
            .await
            .map_err(|error| sqlx::Error::Configuration(Box::new(error)))?;
        SQLITE_MIGRATOR.run(&pool).await?;
        Ok(Self { pool })
    }

    /**
     * Executes the FTS5 branch of sparse retrieval and converts the backend
     * ranking into a stable higher-is-better score.
     */
    async fn search_sparse_fts(
        &self,
        query: &str,
        top_k: i64,
        filters: &SearchFilters,
    ) -> Result<Vec<ScoredChunk>, sqlx::Error> {
        let Some(fts_query) = build_fts5_query(query) else {
            return Ok(Vec::new());
        };

        let mut builder = QueryBuilder::<Sqlite>::new(
            r#"
            SELECT
                c.id AS chunk_id,
                c.content,
                c.chunk_index,
                c.section_title,
                d.file_path,
                d.file_name
            FROM chunks_fts
            JOIN chunks c ON c.rowid = chunks_fts.rowid
            JOIN documents d ON c.document_id = d.id
            WHERE chunks_fts MATCH
            "#,
        );
        builder.push_bind(fts_query);
        push_sqlite_document_filters(&mut builder, filters);
        builder.push(" ORDER BY bm25(chunks_fts, 10.0, 1.0) ASC LIMIT ");
        builder.push_bind(top_k);

        let rows = builder.build().fetch_all(&self.pool).await?;
        rows.into_iter()
            .enumerate()
            .map(|(index, row)| {
                let mut chunk = map_scored_chunk_row(row, 0.0)?;
                chunk.score = 1.0 / (index as f64 + 1.0);
                Ok(chunk)
            })
            .collect()
    }

    /**
     * Executes the trigram-backed sparse branch by preselecting candidates with
     * FTS5 trigram indexes and rescoring them in Rust with Sorensen-Dice.
     */
    async fn search_sparse_trigram(
        &self,
        query: &str,
        top_k: i64,
        filters: &SearchFilters,
    ) -> Result<Vec<ScoredChunk>, sqlx::Error> {
        let like_pattern = format!("%{}%", escape_like_pattern(query));
        let candidate_limit = std::cmp::max(top_k.saturating_mul(5), 20);

        let mut title_builder = QueryBuilder::<Sqlite>::new(
            r#"
            SELECT
                c.id AS chunk_id,
                c.content,
                c.chunk_index,
                c.section_title,
                d.file_path,
                d.file_name
            FROM chunk_titles_trigram
            JOIN chunks c ON c.rowid = chunk_titles_trigram.rowid
            JOIN documents d ON c.document_id = d.id
            WHERE chunk_titles_trigram.section_title LIKE
            "#,
        );
        title_builder.push_bind(like_pattern.clone());
        title_builder.push(" ESCAPE '\\'");
        push_sqlite_document_filters(&mut title_builder, filters);
        title_builder.push(" LIMIT ");
        title_builder.push_bind(candidate_limit);

        let mut path_builder = QueryBuilder::<Sqlite>::new(
            r#"
            SELECT
                c.id AS chunk_id,
                c.content,
                c.chunk_index,
                c.section_title,
                d.file_path,
                d.file_name
            FROM document_paths_trigram
            JOIN documents d ON d.rowid = document_paths_trigram.rowid
            JOIN chunks c ON c.document_id = d.id
            WHERE document_paths_trigram.file_path LIKE
            "#,
        );
        path_builder.push_bind(like_pattern);
        path_builder.push(" ESCAPE '\\'");
        push_sqlite_document_filters(&mut path_builder, filters);
        path_builder.push(" LIMIT ");
        path_builder.push_bind(candidate_limit);

        let title_rows = title_builder.build().fetch_all(&self.pool).await?;
        let path_rows = path_builder.build().fetch_all(&self.pool).await?;

        let mut combined: HashMap<Uuid, ScoredChunk> = HashMap::new();
        for row in title_rows.into_iter().chain(path_rows) {
            let chunk = map_scored_chunk_row(row, 0.0)?;
            let title_score = chunk
                .section_title
                .as_deref()
                .map(|title| trigram_similarity(title, query))
                .unwrap_or(0.0);
            let path_score = trigram_similarity(&chunk.file_path, query);
            let score = title_score.max(path_score);
            if score <= 0.0 {
                continue;
            }

            combined
                .entry(chunk.chunk_id)
                .and_modify(|existing| {
                    if score > existing.score {
                        existing.score = score;
                    }
                })
                .or_insert_with(|| ScoredChunk { score, ..chunk });
        }

        let mut results: Vec<ScoredChunk> = combined.into_values().collect();
        results.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(top_k as usize);
        Ok(results)
    }
}

#[async_trait]
impl StorageBackend for SqliteBackend {
    async fn get_document_by_path(&self, file_path: &str) -> Result<Option<Document>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT id, content_hash, created_at, updated_at FROM documents WHERE file_path = ?",
        )
        .bind(file_path)
        .fetch_optional(&self.pool)
        .await?;

        row.map(map_document_row).transpose()
    }

    async fn upsert_document(&self, input: &DocumentUpsert) -> Result<Uuid, sqlx::Error> {
        let now = Utc::now().to_rfc3339();
        let new_id = Uuid::new_v4().to_string();
        let tags_json = encode_tags(&input.tags)?;

        let row = sqlx::query(
            r#"
            INSERT INTO documents (id, file_path, file_name, file_type, content_hash, project, tags, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(file_path) DO UPDATE SET
                file_name = excluded.file_name,
                content_hash = excluded.content_hash,
                project = excluded.project,
                tags = excluded.tags,
                updated_at = excluded.updated_at
            RETURNING id
            "#,
        )
        .bind(new_id)
        .bind(&input.file_path)
        .bind(&input.file_name)
        .bind(&input.file_type)
        .bind(&input.content_hash)
        .bind(input.project.as_deref())
        .bind(tags_json)
        .bind(&now)
        .bind(&now)
        .fetch_one(&self.pool)
        .await?;

        parse_uuid_text(row.try_get("id")?)
    }

    async fn replace_document_chunks(
        &self,
        document_id: Uuid,
        chunks: &[ChunkInsert],
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            r#"
            DELETE FROM entity_relationships
            WHERE evidence_chunk_id IN (
                SELECT id FROM chunks WHERE document_id = ?
            )
            "#,
        )
        .bind(document_id.to_string())
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM chunks WHERE document_id = ?")
            .bind(document_id.to_string())
            .execute(&mut *tx)
            .await?;

        // PERF-02: multi-row INSERT in batches of CHUNK_INSERT_BATCH_SIZE.
        // Replaces an N-round-trip per-chunk loop with roughly
        // N/50 round-trips while staying well below SQLite's default
        // `SQLITE_LIMIT_VARIABLE_NUMBER = 999` parameter cap (7 params per
        // row x 50 rows = 350 parameters).
        /// Rows per multi-row INSERT batch for chunks.
        const CHUNK_INSERT_BATCH_SIZE: usize = 50;

        for chunk in chunks {
            validate_embedding_dimensions(&chunk.embedding)?;
        }

        let now = Utc::now().to_rfc3339();
        let document_id_text = document_id.to_string();
        for batch in chunks.chunks(CHUNK_INSERT_BATCH_SIZE) {
            let mut builder = sqlx::QueryBuilder::new(
                "INSERT INTO chunks (id, document_id, content, chunk_index, section_title, embedding, created_at) ",
            );
            builder.push_values(batch, |mut row, chunk| {
                row.push_bind(Uuid::new_v4().to_string())
                    .push_bind(&document_id_text)
                    .push_bind(&chunk.content)
                    .push_bind(chunk.chunk_index)
                    .push_bind(chunk.section_title.as_deref())
                    .push_bind(embedding_to_blob(&chunk.embedding))
                    .push_bind(&now);
            });
            builder.build().execute(&mut *tx).await?;
        }

        tx.commit().await
    }

    async fn replace_document_entity_graph(
        &self,
        document_id: Uuid,
        graph: &mut DocumentEntityGraph,
    ) -> Result<(), sqlx::Error> {
        entity_graph::replace_document_entity_graph(&self.pool, document_id, graph).await
    }

    async fn search_graph(
        &self,
        seed_chunk_ids: &[Uuid],
        top_k: i64,
        filters: &SearchFilters,
        traversal_depth: u8,
    ) -> Result<Vec<ScoredChunk>, sqlx::Error> {
        if seed_chunk_ids.is_empty() {
            return Ok(Vec::new());
        }

        let seed_id_strings: Vec<String> = seed_chunk_ids.iter().map(|id| id.to_string()).collect();

        let mut builder = QueryBuilder::<Sqlite>::new("");
        builder.push(
            r#"
            WITH RECURSIVE seed_entities AS (
                SELECT DISTINCT entity_id
                FROM entity_mentions
                WHERE chunk_id IN (
            "#,
        );
        {
            let mut separated = builder.separated(", ");
            for id in &seed_id_strings {
                separated.push_bind(id.clone());
            }
        }
        builder.push(
            r#"
            )),
            graph_walk AS (
                SELECT
                    entity_id,
                    0 AS depth,
                    entity_id AS path_start,
                    1.0 AS score
                FROM seed_entities
                UNION ALL
                SELECT
                    CASE
                        WHEN r.source_entity_id = w.entity_id THEN r.target_entity_id
                        ELSE r.source_entity_id
                    END AS entity_id,
                    w.depth + 1 AS depth,
                    w.path_start,
                    w.score * (
                        MAX(COALESCE(r.weight, 1.0), 0.1)
                        / CAST((w.depth + 2) AS REAL)
                    ) AS score
                FROM graph_walk AS w
                JOIN entity_relationships AS r
                    ON r.source_entity_id = w.entity_id
                    OR r.target_entity_id = w.entity_id
                WHERE w.depth <
            "#,
        );
        builder.push_bind(i32::from(traversal_depth));
        builder.push(
            r#"
            )
            SELECT
                c.id AS chunk_id,
                c.content,
                c.chunk_index,
                c.section_title,
                d.file_path,
                d.file_name,
                MAX(gw.score) AS score
            FROM graph_walk AS gw
            JOIN entity_mentions AS m
                ON m.entity_id = gw.entity_id
            JOIN chunks AS c
                ON c.id = m.chunk_id
            JOIN documents AS d
                ON d.id = c.document_id
            WHERE gw.depth > 0
                AND c.id NOT IN (
            "#,
        );
        {
            let mut separated = builder.separated(", ");
            for id in &seed_id_strings {
                separated.push_bind(id.clone());
            }
        }
        builder.push(")");
        push_sqlite_document_filters(&mut builder, filters);
        builder.push(
            " GROUP BY c.id, c.content, c.chunk_index, c.section_title, d.file_path, d.file_name ORDER BY score DESC LIMIT ",
        );
        builder.push_bind(top_k);

        let rows = builder.build().fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                let score: f64 = row.try_get("score")?;
                map_scored_chunk_row(row, score)
            })
            .collect()
    }

    async fn search_dense(
        &self,
        query_embedding: &[f32],
        top_k: i64,
        filters: &SearchFilters,
    ) -> Result<Vec<ScoredChunk>, sqlx::Error> {
        validate_embedding_dimensions(query_embedding)?;

        let mut builder = QueryBuilder::<Sqlite>::new(
            r#"
            SELECT
                c.id AS chunk_id,
                c.content,
                c.chunk_index,
                c.section_title,
                d.file_path,
                d.file_name,
                c.embedding
            FROM chunks c
            JOIN documents d ON c.document_id = d.id
            WHERE 1 = 1
            "#,
        );
        push_sqlite_document_filters(&mut builder, filters);

        let rows = builder.build().fetch_all(&self.pool).await?;
        let mut results: Vec<ScoredChunk> = rows
            .into_iter()
            .map(|row| {
                let embedding_blob: Vec<u8> = row.try_get("embedding")?;
                let score =
                    cosine_similarity(query_embedding, &decode_embedding_blob(&embedding_blob)?)?;
                map_scored_chunk_row(row, score)
            })
            .collect::<Result<Vec<_>, _>>()?;

        results.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(top_k as usize);
        Ok(results)
    }

    async fn search_sparse(
        &self,
        query: &str,
        top_k: i64,
        filters: &SearchFilters,
    ) -> Result<Vec<ScoredChunk>, sqlx::Error> {
        let (fts_results, trigram_results) = tokio::try_join!(
            self.search_sparse_fts(query, top_k, filters),
            self.search_sparse_trigram(query, top_k, filters),
        )?;

        let mut combined: HashMap<Uuid, ScoredChunk> = HashMap::new();
        for chunk in fts_results.into_iter().chain(trigram_results) {
            combined
                .entry(chunk.chunk_id)
                .and_modify(|existing| {
                    if chunk.score > existing.score {
                        existing.score = chunk.score;
                    }
                })
                .or_insert(chunk);
        }

        let mut results: Vec<ScoredChunk> = combined.into_values().collect();
        results.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(top_k as usize);
        Ok(results)
    }

    async fn list_documents(
        &self,
        project: Option<&str>,
        file_type: Option<&str>,
    ) -> Result<Vec<DocumentWithChunkCount>, sqlx::Error> {
        let mut builder = QueryBuilder::<Sqlite>::new(
            r#"
            SELECT
                d.id,
                d.file_path,
                d.file_name,
                d.file_type,
                d.project,
                d.tags,
                d.created_at,
                d.updated_at,
                COUNT(c.id) AS chunk_count
            FROM documents d
            LEFT JOIN chunks c ON c.document_id = d.id
            WHERE 1 = 1
            "#,
        );

        if let Some(project) = project {
            builder.push(" AND d.project = ");
            builder.push_bind(project);
        }

        if let Some(file_type) = file_type {
            builder.push(" AND d.file_type = ");
            builder.push_bind(file_type);
        }

        builder.push(" GROUP BY d.id ORDER BY d.updated_at DESC");

        let rows = builder.build().fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(map_document_with_chunk_count_row)
            .collect()
    }

    async fn get_document_chunks(
        &self,
        file_path: &str,
    ) -> Result<Vec<DocumentChunk>, sqlx::Error> {
        let rows = sqlx::query(
            r#"
            SELECT
                c.content,
                c.chunk_index,
                c.section_title
            FROM chunks c
            JOIN documents d ON d.id = c.document_id
            WHERE d.file_path = ?
            ORDER BY c.chunk_index ASC
            "#,
        )
        .bind(file_path)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(map_document_chunk_row).collect()
    }

    async fn get_chunks_by_index(
        &self,
        file_path: Option<&str>,
        document_id: Option<Uuid>,
        start_index: i32,
        end_index: i32,
    ) -> Result<Vec<DocumentChunkWithMeta>, sqlx::Error> {
        if file_path.is_some() == document_id.is_some() {
            return Ok(Vec::new());
        }
        if end_index < start_index {
            return Ok(Vec::new());
        }

        let document_id_text = document_id.map(|id| id.to_string());
        let rows = sqlx::query(
            r#"
            SELECT
                d.id          AS document_id,
                d.file_path   AS file_path,
                d.file_name   AS file_name,
                c.chunk_index AS chunk_index,
                c.section_title AS section_title,
                c.content     AS content
            FROM chunks c
            JOIN documents d ON d.id = c.document_id
            WHERE (? IS NULL OR d.file_path = ?)
              AND (? IS NULL OR d.id = ?)
              AND c.chunk_index BETWEEN ? AND ?
            ORDER BY c.chunk_index ASC
            "#,
        )
        .bind(file_path)
        .bind(file_path)
        .bind(document_id_text.as_deref())
        .bind(document_id_text.as_deref())
        .bind(start_index)
        .bind(end_index)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(map_document_chunk_with_meta_row)
            .collect()
    }

    async fn delete_document(
        &self,
        file_path: Option<&str>,
        document_id: Option<Uuid>,
    ) -> Result<Option<(String, i64)>, sqlx::Error> {
        let document_row = if let Some(document_id) = document_id {
            sqlx::query("SELECT id, file_path FROM documents WHERE id = ?")
                .bind(document_id.to_string())
                .fetch_optional(&self.pool)
                .await?
        } else if let Some(file_path) = file_path {
            sqlx::query("SELECT id, file_path FROM documents WHERE file_path = ?")
                .bind(file_path)
                .fetch_optional(&self.pool)
                .await?
        } else {
            return Ok(None);
        };

        let Some(document_row) = document_row else {
            return Ok(None);
        };

        let document_id = document_row.try_get::<String, _>("id")?;
        let path = document_row.try_get::<String, _>("file_path")?;

        let chunk_count =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM chunks WHERE document_id = ?")
                .bind(&document_id)
                .fetch_one(&self.pool)
                .await?;

        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
            DELETE FROM entity_relationships
            WHERE evidence_chunk_id IN (
                SELECT id FROM chunks WHERE document_id = ?
            )
            "#,
        )
        .bind(&document_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM documents WHERE id = ?")
            .bind(&document_id)
            .execute(&mut *tx)
            .await?;
        prune_orphan_entities(&mut tx).await?;
        tx.commit().await?;

        Ok(Some((path, chunk_count)))
    }

    async fn save_communities(
        &self,
        project: Option<&str>,
        communities: &[CommunityWithSummary],
    ) -> Result<(), sqlx::Error> {
        community::save_communities(&self.pool, project, communities).await
    }

    async fn delete_project_communities(&self, project: Option<&str>) -> Result<(), sqlx::Error> {
        community::delete_project_communities(&self.pool, project).await
    }

    async fn get_communities_for_entities(
        &self,
        project: Option<&str>,
        entity_ids: &[Uuid],
    ) -> Result<Vec<StoredCommunity>, sqlx::Error> {
        community::get_communities_for_entities(&self.pool, project, entity_ids).await
    }

    async fn search_communities_by_embedding(
        &self,
        project: Option<&str>,
        query_embedding: &[f32],
        top_k: i64,
    ) -> Result<Vec<StoredCommunity>, sqlx::Error> {
        community::search_communities_by_embedding(&self.pool, project, query_embedding, top_k)
            .await
    }

    async fn list_communities(
        &self,
        project: Option<&str>,
    ) -> Result<Vec<StoredCommunity>, sqlx::Error> {
        community::list_communities(&self.pool, project).await
    }
}
