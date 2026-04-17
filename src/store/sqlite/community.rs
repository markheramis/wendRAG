/*!
 * Community persistence for the SQLite backend.
 *
 * Stores community embeddings as BLOBs and computes cosine similarity in Rust
 * for ANN search (efficient for <1000 communities). Reuses the embedding
 * encode/decode utilities from the `embeddings` sibling module.
 */

use chrono::Utc;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::entity::CommunityWithSummary;
use crate::store::models::StoredCommunity;

use super::embeddings::{cosine_similarity, decode_embedding_blob, embedding_to_blob};

/**
 * Inserts communities and their entity member links in a single transaction.
 * Community summary embeddings are stored as compact BLOBs.
 */
pub(crate) async fn save_communities(
    pool: &SqlitePool,
    project: Option<&str>,
    communities: &[CommunityWithSummary],
) -> Result<(), sqlx::Error> {
    if communities.is_empty() {
        return Ok(());
    }

    let mut tx = pool.begin().await?;
    let now = Utc::now().to_rfc3339();

    for community in communities {
        let community_id = Uuid::new_v4().to_string();
        let embedding_blob = community
            .summary_embedding
            .as_ref()
            .map(|e| embedding_to_blob(e));

        sqlx::query(
            r#"
            INSERT INTO entity_communities (id, name, summary, project, importance, embedding, created_at)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&community_id)
        .bind(&community.community.name)
        .bind(community.summary.as_deref())
        .bind(project)
        .bind(community.community.importance)
        .bind(embedding_blob)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        let entity_ids = resolve_entity_ids(&mut tx, &community.community.entity_ids).await?;

        // PERF-02: batch community_members inserts with multi-row INSERT.
        /// Rows per batch. 2 params per row stays well below SQLite's
        /// default parameter limit of 999.
        const MEMBER_BATCH_SIZE: usize = 100;

        for batch in entity_ids.chunks(MEMBER_BATCH_SIZE) {
            let mut builder = sqlx::QueryBuilder::new(
                "INSERT INTO community_members (community_id, entity_id) ",
            );
            builder.push_values(batch, |mut row, entity_id| {
                row.push_bind(&community_id).push_bind(entity_id);
            });
            builder.push(" ON CONFLICT DO NOTHING");
            builder.build().execute(&mut *tx).await?;
        }
    }

    tx.commit().await
}

/**
 * Removes all communities scoped to a project so they can be re-detected.
 * CASCADE deletes community_members rows.
 */
pub(crate) async fn delete_project_communities(
    pool: &SqlitePool,
    project: Option<&str>,
) -> Result<(), sqlx::Error> {
    match project {
        Some(p) => {
            sqlx::query("DELETE FROM entity_communities WHERE project = ?")
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
    pool: &SqlitePool,
    project: Option<&str>,
    entity_ids: &[Uuid],
) -> Result<Vec<StoredCommunity>, sqlx::Error> {
    if entity_ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders: String = entity_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");

    let sql = format!(
        r#"
        SELECT ec.id, ec.name, ec.summary, ec.project, ec.importance,
               (SELECT COUNT(*) FROM community_members cm2 WHERE cm2.community_id = ec.id) AS entity_count
        FROM entity_communities ec
        JOIN community_members cm ON cm.community_id = ec.id
        WHERE cm.entity_id IN ({placeholders})
          AND (ec.project = ? OR ec.project IS NULL)
        GROUP BY ec.id
        ORDER BY ec.importance DESC
        "#,
    );

    let mut query = sqlx::query(&sql);
    for id in entity_ids {
        query = query.bind(id.to_string());
    }
    query = query.bind(project);

    let rows = query.fetch_all(pool).await?;
    Ok(rows.iter().map(sqlite_row_to_community).collect())
}

/**
 * In-memory cosine similarity scan over community embeddings. Loads all
 * community embeddings for the project, scores them, and returns top-k.
 * Efficient for <1000 communities.
 */
pub(crate) async fn search_communities_by_embedding(
    pool: &SqlitePool,
    project: Option<&str>,
    query_embedding: &[f32],
    top_k: i64,
) -> Result<Vec<StoredCommunity>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT ec.id, ec.name, ec.summary, ec.project, ec.importance, ec.embedding,
               (SELECT COUNT(*) FROM community_members cm WHERE cm.community_id = ec.id) AS entity_count
        FROM entity_communities ec
        WHERE ec.embedding IS NOT NULL
          AND (ec.project = ? OR ec.project IS NULL)
        "#,
    )
    .bind(project)
    .fetch_all(pool)
    .await?;

    let mut scored: Vec<(f64, StoredCommunity)> = rows
        .iter()
        .filter_map(|row| {
            let blob: Vec<u8> = row.get("embedding");
            let embedding = decode_embedding_blob(&blob).ok()?;
            let similarity = cosine_similarity(query_embedding, &embedding).ok()?;
            Some((similarity, sqlite_row_to_community(row)))
        })
        .collect();

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(top_k as usize);

    Ok(scored.into_iter().map(|(_, c)| c).collect())
}

/** Lists all communities for a project scope (plus globals). */
pub(crate) async fn list_communities(
    pool: &SqlitePool,
    project: Option<&str>,
) -> Result<Vec<StoredCommunity>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT ec.id, ec.name, ec.summary, ec.project, ec.importance,
               (SELECT COUNT(*) FROM community_members cm WHERE cm.community_id = ec.id) AS entity_count
        FROM entity_communities ec
        WHERE (? IS NULL OR ec.project = ? OR ec.project IS NULL)
        ORDER BY ec.importance DESC
        "#,
    )
    .bind(project)
    .bind(project)
    .fetch_all(pool)
    .await?;

    Ok(rows.iter().map(sqlite_row_to_community).collect())
}

/**
 * Resolves entity normalized names to their database TEXT IDs. Entities that
 * are not found (e.g. pruned as orphans) are silently skipped.
 */
async fn resolve_entity_ids(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    entity_names: &[String],
) -> Result<Vec<String>, sqlx::Error> {
    if entity_names.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders: String = entity_names.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!("SELECT id FROM entities WHERE normalized_name IN ({placeholders})");
    let mut query = sqlx::query(&sql);
    for name in entity_names {
        query = query.bind(name);
    }

    let rows = query.fetch_all(&mut **tx).await?;
    Ok(rows.iter().map(|r| r.get::<String, _>("id")).collect())
}

fn sqlite_row_to_community(row: &sqlx::sqlite::SqliteRow) -> StoredCommunity {
    let id_str: String = row.get("id");
    StoredCommunity {
        id: Uuid::parse_str(&id_str).unwrap_or_default(),
        name: row.get("name"),
        summary: row.get("summary"),
        project: row.get("project"),
        importance: row.get("importance"),
        entity_count: row.get("entity_count"),
    }
}
