/**
 * Community persistence for the PostgreSQL backend.
 *
 * Uses pgvector HNSW index for ANN search over community summary embeddings
 * and batch INSERT via QueryBuilder for efficient member link creation.
 */

use pgvector::Vector;
use sqlx::PgPool;
use uuid::Uuid;

use crate::entity::CommunityWithSummary;
use crate::store::models::StoredCommunity;

const BATCH_SIZE: usize = 50;

/**
 * Inserts communities and their entity member links in a single transaction.
 * Community embeddings are stored as pgvector for HNSW-indexed ANN search.
 */
pub(crate) async fn save_communities(
    pool: &PgPool,
    project: Option<&str>,
    communities: &[CommunityWithSummary],
) -> Result<(), sqlx::Error> {
    if communities.is_empty() {
        return Ok(());
    }

    let mut tx = pool.begin().await?;

    for community in communities {
        let embedding = community
            .summary_embedding
            .as_ref()
            .map(|e| Vector::from(e.clone()));

        let (community_id,): (Uuid,) = sqlx::query_as(
            r#"
            INSERT INTO entity_communities (name, summary, project, importance, embedding)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING id
            "#,
        )
        .bind(&community.community.name)
        .bind(community.summary.as_deref())
        .bind(project)
        .bind(community.community.importance)
        .bind(embedding)
        .fetch_one(&mut *tx)
        .await?;

        let entity_ids: Vec<Uuid> = resolve_entity_ids(&mut tx, &community.community.entity_ids).await?;

        for batch in entity_ids.chunks(BATCH_SIZE) {
            let mut builder = sqlx::QueryBuilder::new(
                "INSERT INTO community_members (community_id, entity_id) ",
            );
            builder.push_values(batch, |mut row, entity_id| {
                row.push_bind(community_id).push_bind(*entity_id);
            });
            builder.push(" ON CONFLICT DO NOTHING");
            builder.build().execute(&mut *tx).await?;
        }
    }

    tx.commit().await
}

/**
 * Removes all communities scoped to a project so they can be re-detected
 * from the current entity graph. CASCADE deletes community_members rows.
 */
pub(crate) async fn delete_project_communities(
    pool: &PgPool,
    project: Option<&str>,
) -> Result<(), sqlx::Error> {
    match project {
        Some(p) => {
            sqlx::query("DELETE FROM entity_communities WHERE project = $1")
                .bind(p)
                .execute(pool)
                .await?;
        }
        None => {
            sqlx::query("DELETE FROM entity_communities WHERE project IS NULL")
                .execute(pool)
                .await?;
        }
    }
    Ok(())
}

/**
 * Finds communities containing any of the given entity IDs (local-tier).
 * Also returns global communities (project IS NULL) when project is specified.
 */
pub(crate) async fn get_communities_for_entities(
    pool: &PgPool,
    project: Option<&str>,
    entity_ids: &[Uuid],
) -> Result<Vec<StoredCommunity>, sqlx::Error> {
    if entity_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, (Uuid, String, Option<String>, Option<String>, f32, i64)>(
        r#"
        SELECT ec.id, ec.name, ec.summary, ec.project, ec.importance::real,
               COUNT(cm2.entity_id) AS entity_count
        FROM entity_communities ec
        JOIN community_members cm ON cm.community_id = ec.id
        LEFT JOIN community_members cm2 ON cm2.community_id = ec.id
        WHERE cm.entity_id = ANY($1)
          AND (ec.project = $2 OR ec.project IS NULL)
        GROUP BY ec.id
        ORDER BY ec.importance DESC
        "#,
    )
    .bind(entity_ids)
    .bind(project)
    .fetch_all(pool)
    .await
    .map(|rows| rows.into_iter().map(row_to_stored_community).collect())
}

/**
 * ANN search over community summary embeddings for global-tier retrieval.
 * Returns top-k communities closest to the query embedding.
 */
pub(crate) async fn search_communities_by_embedding(
    pool: &PgPool,
    project: Option<&str>,
    query_embedding: &[f32],
    top_k: i64,
) -> Result<Vec<StoredCommunity>, sqlx::Error> {
    let query_vec = Vector::from(query_embedding.to_vec());

    sqlx::query_as::<_, (Uuid, String, Option<String>, Option<String>, f32, i64)>(
        r#"
        SELECT ec.id, ec.name, ec.summary, ec.project, ec.importance::real,
               (SELECT COUNT(*) FROM community_members cm WHERE cm.community_id = ec.id) AS entity_count
        FROM entity_communities ec
        WHERE ec.embedding IS NOT NULL
          AND (ec.project = $1 OR ec.project IS NULL)
        ORDER BY ec.embedding <=> $2
        LIMIT $3
        "#,
    )
    .bind(project)
    .bind(query_vec)
    .bind(top_k)
    .fetch_all(pool)
    .await
    .map(|rows| rows.into_iter().map(row_to_stored_community).collect())
}

/** Lists all communities for a project scope (plus globals). */
pub(crate) async fn list_communities(
    pool: &PgPool,
    project: Option<&str>,
) -> Result<Vec<StoredCommunity>, sqlx::Error> {
    sqlx::query_as::<_, (Uuid, String, Option<String>, Option<String>, f32, i64)>(
        r#"
        SELECT ec.id, ec.name, ec.summary, ec.project, ec.importance::real,
               (SELECT COUNT(*) FROM community_members cm WHERE cm.community_id = ec.id) AS entity_count
        FROM entity_communities ec
        WHERE ($1::text IS NULL OR ec.project = $1 OR ec.project IS NULL)
        ORDER BY ec.importance DESC
        "#,
    )
    .bind(project)
    .fetch_all(pool)
    .await
    .map(|rows| rows.into_iter().map(row_to_stored_community).collect())
}

/**
 * Resolves entity normalized names to their database UUIDs. Entities that
 * are not found (e.g. pruned as orphans) are silently skipped.
 */
async fn resolve_entity_ids(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    entity_names: &[String],
) -> Result<Vec<Uuid>, sqlx::Error> {
    if entity_names.is_empty() {
        return Ok(Vec::new());
    }

    let rows = sqlx::query_as::<_, (Uuid,)>(
        "SELECT id FROM entities WHERE normalized_name = ANY($1)",
    )
    .bind(entity_names)
    .fetch_all(&mut **tx)
    .await?;

    Ok(rows.into_iter().map(|(id,)| id).collect())
}

fn row_to_stored_community(
    row: (Uuid, String, Option<String>, Option<String>, f32, i64),
) -> StoredCommunity {
    StoredCommunity {
        id: row.0,
        name: row.1,
        summary: row.2,
        project: row.3,
        importance: row.4,
        entity_count: row.5,
    }
}
