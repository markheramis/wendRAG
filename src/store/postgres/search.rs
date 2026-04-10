/**
 * Dense, sparse, and graph search implementations for the PostgreSQL backend.
 */

use std::collections::HashMap;

use pgvector::Vector;
use sqlx::PgPool;
use uuid::Uuid;

use crate::retrieve::ScoredChunk;
use crate::store::SearchFilters;

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct DenseSearchRow {
    pub chunk_id: Uuid,
    pub content: String,
    pub chunk_index: i32,
    pub section_title: Option<String>,
    pub file_path: String,
    pub file_name: String,
    pub score: f64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct SparseSearchRow {
    pub chunk_id: Uuid,
    pub content: String,
    pub chunk_index: i32,
    pub section_title: Option<String>,
    pub file_path: String,
    pub file_name: String,
    pub score: f32,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct GraphSearchRow {
    pub chunk_id: Uuid,
    pub content: String,
    pub chunk_index: i32,
    pub section_title: Option<String>,
    pub file_path: String,
    pub file_name: String,
    pub score: f64,
}

/**
 * Executes pgvector cosine search and maps the results into shared chunk
 * score objects.
 */
pub(crate) async fn search_dense(
    pool: &PgPool,
    query_embedding: &[f32],
    top_k: i64,
    filters: &SearchFilters,
) -> Result<Vec<ScoredChunk>, sqlx::Error> {
    let rows = sqlx::query_as::<_, DenseSearchRow>(
        r#"
        SELECT
            c.id AS chunk_id,
            c.content,
            c.chunk_index,
            c.section_title,
            d.file_path,
            d.file_name,
            (1.0 - (c.embedding <=> $1))::float8 AS score
        FROM chunks c
        JOIN documents d ON c.document_id = d.id
        WHERE ($2::text IS NULL OR d.project = $2)
          AND ($3::text[] IS NULL OR d.file_type = ANY($3))
          AND ($4::text[] IS NULL OR d.tags && $4)
        ORDER BY c.embedding <=> $1
        LIMIT $5
        "#,
    )
    .bind(Vector::from(query_embedding.to_vec()))
    .bind(filters.project.as_deref())
    .bind(filters.file_types.as_deref())
    .bind(filters.tags.as_deref())
    .bind(top_k)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| ScoredChunk {
            chunk_id: row.chunk_id,
            content: row.content,
            section_title: row.section_title,
            file_path: row.file_path,
            file_name: row.file_name,
            chunk_index: row.chunk_index,
            score: row.score,
        })
        .collect())
}

/**
 * Executes the PostgreSQL full-text branch used by sparse retrieval.
 */
pub(crate) async fn search_sparse_fts(
    pool: &PgPool,
    query: &str,
    top_k: i64,
    filters: &SearchFilters,
) -> Result<Vec<SparseSearchRow>, sqlx::Error> {
    sqlx::query_as::<_, SparseSearchRow>(
        r#"
        SELECT
            c.id AS chunk_id,
            c.content,
            c.chunk_index,
            c.section_title,
            d.file_path,
            d.file_name,
            ts_rank(c.search_tsv, plainto_tsquery('english', $1)) AS score
        FROM chunks c
        JOIN documents d ON c.document_id = d.id
        WHERE c.search_tsv @@ plainto_tsquery('english', $1)
          AND ($2::text IS NULL OR d.project = $2)
          AND ($3::text[] IS NULL OR d.file_type = ANY($3))
          AND ($4::text[] IS NULL OR d.tags && $4)
        ORDER BY score DESC
        LIMIT $5
        "#,
    )
    .bind(query)
    .bind(filters.project.as_deref())
    .bind(filters.file_types.as_deref())
    .bind(filters.tags.as_deref())
    .bind(top_k)
    .fetch_all(pool)
    .await
}

/**
 * Executes the PostgreSQL trigram branch used by sparse retrieval.
 */
pub(crate) async fn search_sparse_trigram(
    pool: &PgPool,
    query: &str,
    top_k: i64,
    filters: &SearchFilters,
) -> Result<Vec<SparseSearchRow>, sqlx::Error> {
    sqlx::query_as::<_, SparseSearchRow>(
        r#"
        SELECT
            c.id AS chunk_id,
            c.content,
            c.chunk_index,
            c.section_title,
            d.file_path,
            d.file_name,
            GREATEST(
                coalesce(similarity(c.section_title, $1), 0),
                similarity(d.file_path, $1)
            ) AS score
        FROM chunks c
        JOIN documents d ON c.document_id = d.id
        WHERE (c.section_title % $1 OR d.file_path % $1)
          AND ($2::text IS NULL OR d.project = $2)
          AND ($3::text[] IS NULL OR d.file_type = ANY($3))
          AND ($4::text[] IS NULL OR d.tags && $4)
        ORDER BY score DESC
        LIMIT $5
        "#,
    )
    .bind(query)
    .bind(filters.project.as_deref())
    .bind(filters.file_types.as_deref())
    .bind(filters.tags.as_deref())
    .bind(top_k)
    .fetch_all(pool)
    .await
}

/**
 * Merges PostgreSQL full-text and trigram branches into the sparse result
 * list consumed by sparse-only and hybrid retrieval.
 */
pub(crate) async fn search_sparse(
    pool: &PgPool,
    query: &str,
    top_k: i64,
    filters: &SearchFilters,
) -> Result<Vec<ScoredChunk>, sqlx::Error> {
    let (fts_rows, trigram_rows) = tokio::try_join!(
        search_sparse_fts(pool, query, top_k, filters),
        search_sparse_trigram(pool, query, top_k, filters),
    )?;

    let mut combined: HashMap<Uuid, ScoredChunk> = HashMap::new();

    for row in fts_rows.into_iter().chain(trigram_rows) {
        combined
            .entry(row.chunk_id)
            .and_modify(|existing| {
                let next_score = row.score as f64;
                if next_score > existing.score {
                    existing.score = next_score;
                }
            })
            .or_insert_with(|| ScoredChunk {
                chunk_id: row.chunk_id,
                content: row.content,
                section_title: row.section_title,
                file_path: row.file_path,
                file_name: row.file_name,
                chunk_index: row.chunk_index,
                score: row.score as f64,
            });
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

/**
 * Expands hybrid seed chunks through the PostgreSQL entity graph and returns
 * related chunks ranked by recursive traversal score.
 */
pub(crate) async fn search_graph(
    pool: &PgPool,
    seed_chunk_ids: &[Uuid],
    top_k: i64,
    filters: &SearchFilters,
    traversal_depth: u8,
) -> Result<Vec<ScoredChunk>, sqlx::Error> {
    if seed_chunk_ids.is_empty() {
        return Ok(Vec::new());
    }

    let rows = sqlx::query_as::<_, GraphSearchRow>(
        r#"
        WITH RECURSIVE seed_entities AS (
            SELECT DISTINCT entity_id
            FROM entity_mentions
            WHERE chunk_id = ANY($1::uuid[])
        ),
        graph_walk AS (
            SELECT
                entity_id,
                0::integer AS depth,
                ARRAY[entity_id] AS path,
                1.0::float8 AS score
            FROM seed_entities
            UNION ALL
            SELECT
                CASE
                    WHEN relationship.source_entity_id = walk.entity_id THEN relationship.target_entity_id
                    ELSE relationship.source_entity_id
                END AS entity_id,
                walk.depth + 1 AS depth,
                walk.path || CASE
                    WHEN relationship.source_entity_id = walk.entity_id THEN relationship.target_entity_id
                    ELSE relationship.source_entity_id
                END AS path,
                walk.score * (
                    GREATEST(COALESCE(relationship.weight, 1.0)::float8, 0.1)
                    / ((walk.depth + 2)::float8)
                ) AS score
            FROM graph_walk AS walk
            JOIN entity_relationships AS relationship
                ON relationship.source_entity_id = walk.entity_id
                OR relationship.target_entity_id = walk.entity_id
            WHERE walk.depth < $2
                AND NOT (
                    CASE
                        WHEN relationship.source_entity_id = walk.entity_id THEN relationship.target_entity_id
                        ELSE relationship.source_entity_id
                    END = ANY(walk.path)
                )
        )
        SELECT
            chunk.id AS chunk_id,
            chunk.content,
            chunk.chunk_index,
            chunk.section_title,
            document.file_path,
            document.file_name,
            MAX(graph_walk.score) AS score
        FROM graph_walk
        JOIN entity_mentions AS mention
            ON mention.entity_id = graph_walk.entity_id
        JOIN chunks AS chunk
            ON chunk.id = mention.chunk_id
        JOIN documents AS document
            ON document.id = chunk.document_id
        WHERE graph_walk.depth > 0
            AND NOT (chunk.id = ANY($1::uuid[]))
            AND ($3::text IS NULL OR document.project = $3)
            AND ($4::text[] IS NULL OR document.file_type = ANY($4))
            AND ($5::text[] IS NULL OR document.tags && $5)
        GROUP BY
            chunk.id,
            chunk.content,
            chunk.chunk_index,
            chunk.section_title,
            document.file_path,
            document.file_name
        ORDER BY score DESC
        LIMIT $6
        "#,
    )
    .bind(seed_chunk_ids)
    .bind(i32::from(traversal_depth))
    .bind(filters.project.as_deref())
    .bind(filters.file_types.as_deref())
    .bind(filters.tags.as_deref())
    .bind(top_k)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| ScoredChunk {
            chunk_id: row.chunk_id,
            content: row.content,
            section_title: row.section_title,
            file_path: row.file_path,
            file_name: row.file_name,
            chunk_index: row.chunk_index,
            score: row.score,
        })
        .collect())
}
