/*!
 * Entity graph persistence and orphan pruning for the SQLite backend.
 */

use std::collections::HashMap;

use chrono::Utc;
use sqlx::Row;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::entity::DocumentEntityGraph;

use super::embeddings::embedding_to_blob;

/**
 * Persists the deduplicated entity graph for a document after its chunks
 * have been written. Mirrors the PostgreSQL implementation using TEXT ids
 * and BLOB embeddings.
 */
pub(crate) async fn replace_document_entity_graph(
    pool: &SqlitePool,
    document_id: Uuid,
    graph: &mut DocumentEntityGraph,
) -> Result<(), sqlx::Error> {
    if graph.is_empty() {
        let mut tx = pool.begin().await?;
        prune_orphan_entities(&mut tx).await?;
        tx.commit().await?;
        return Ok(());
    }

    let mut tx = pool.begin().await?;
    let chunk_rows: Vec<(String, i32)> =
        sqlx::query("SELECT id, chunk_index FROM chunks WHERE document_id = ?")
            .bind(document_id.to_string())
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .map(|row| {
                Ok::<_, sqlx::Error>((
                    row.try_get::<String, _>("id")?,
                    row.try_get::<i32, _>("chunk_index")?,
                ))
            })
            .collect::<Result<Vec<_>, _>>()?;

    let chunk_ids_by_index: HashMap<i32, String> = chunk_rows
        .into_iter()
        .map(|(id, index)| (index, id))
        .collect();

    let mut entity_ids_by_key: HashMap<(String, String), String> = HashMap::new();
    let now = Utc::now().to_rfc3339();

    // SQLite already passes `&entity.embedding` to `embedding_to_blob` without
    // an intermediate clone, so no `mem::take` is needed here -- the `&mut`
    // receiver exists only to match the updated `StorageBackend` trait
    // signature introduced for the Postgres optimisation.
    for entity in &graph.entities {
        let new_id = Uuid::new_v4().to_string();
        let embedding_blob = if entity.embedding.is_empty() {
            None
        } else {
            Some(embedding_to_blob(&entity.embedding))
        };

        let row = sqlx::query(
            r#"
            INSERT INTO entities (
                id,
                normalized_name,
                name,
                entity_type,
                description,
                embedding,
                mention_count,
                created_at
            )
            VALUES (?, ?, ?, ?, ?, ?, 0, ?)
            ON CONFLICT (normalized_name, entity_type) DO UPDATE
            SET
                name = excluded.name,
                description = COALESCE(excluded.description, entities.description),
                embedding = excluded.embedding
            RETURNING id
            "#,
        )
        .bind(&new_id)
        .bind(&entity.normalized_name)
        .bind(&entity.display_name)
        .bind(&entity.entity_type)
        .bind(entity.description.as_deref())
        .bind(embedding_blob)
        .bind(&now)
        .fetch_one(&mut *tx)
        .await?;

        let entity_id: String = row.try_get("id")?;
        entity_ids_by_key.insert(
            (entity.normalized_name.clone(), entity.entity_type.clone()),
            entity_id,
        );
    }

    // PERF-02: batch mentions and relationships with multi-row INSERTs
    // mirroring the Postgres backend's `QueryBuilder::push_values` pattern.
    /// Rows per multi-row INSERT batch. Stays comfortably below SQLite's
    /// default `SQLITE_LIMIT_VARIABLE_NUMBER = 999` parameter cap even at
    /// 8 params per row (relationships) x 50 rows = 400 parameters.
    const BATCH_SIZE: usize = 50;

    let resolved_mentions: Vec<(String, String)> = graph
        .mentions
        .iter()
        .filter_map(|mention| {
            let chunk_id = chunk_ids_by_index.get(&mention.chunk_index)?.clone();
            let entity_id = entity_ids_by_key
                .get(&(mention.normalized_name.clone(), mention.entity_type.clone()))?
                .clone();
            Some((chunk_id, entity_id))
        })
        .collect();

    for batch in resolved_mentions.chunks(BATCH_SIZE) {
        let mut builder =
            sqlx::QueryBuilder::new("INSERT INTO entity_mentions (chunk_id, entity_id) ");
        builder.push_values(batch, |mut row, (chunk_id, entity_id)| {
            row.push_bind(chunk_id).push_bind(entity_id);
        });
        builder.push(" ON CONFLICT DO NOTHING");
        builder.build().execute(&mut *tx).await?;
    }

    struct ResolvedRelationship<'a> {
        id: String,
        source_entity_id: String,
        target_entity_id: String,
        relationship_type: &'a str,
        description: Option<&'a str>,
        weight: f64,
        evidence_chunk_id: Option<String>,
    }

    let resolved_relationships: Vec<ResolvedRelationship<'_>> = graph
        .relationships
        .iter()
        .filter_map(|relationship| {
            let source_entity_id = entity_ids_by_key
                .get(&(
                    relationship.source_normalized_name.clone(),
                    relationship.source_type.clone(),
                ))?
                .clone();
            let target_entity_id = entity_ids_by_key
                .get(&(
                    relationship.target_normalized_name.clone(),
                    relationship.target_type.clone(),
                ))?
                .clone();
            let evidence_chunk_id = chunk_ids_by_index
                .get(&relationship.evidence_chunk_index)
                .cloned();
            Some(ResolvedRelationship {
                id: Uuid::new_v4().to_string(),
                source_entity_id,
                target_entity_id,
                relationship_type: &relationship.relationship_type,
                description: relationship.description.as_deref(),
                weight: relationship.weight as f64,
                evidence_chunk_id,
            })
        })
        .collect();

    for batch in resolved_relationships.chunks(BATCH_SIZE) {
        let mut builder = sqlx::QueryBuilder::new(
            "INSERT INTO entity_relationships (id, source_entity_id, target_entity_id, relationship_type, description, weight, evidence_chunk_id, created_at) ",
        );
        builder.push_values(batch, |mut row, rel| {
            row.push_bind(&rel.id)
                .push_bind(&rel.source_entity_id)
                .push_bind(&rel.target_entity_id)
                .push_bind(rel.relationship_type)
                .push_bind(rel.description)
                .push_bind(rel.weight)
                .push_bind(rel.evidence_chunk_id.as_deref())
                .push_bind(&now);
        });
        builder.build().execute(&mut *tx).await?;
    }

    prune_orphan_entities(&mut tx).await?;
    tx.commit().await
}

/**
 * Recomputes mention counts and removes orphaned entities that are no longer
 * referenced by any chunk in the SQLite backend.
 */
pub(crate) async fn prune_orphan_entities(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE entities
        SET mention_count = (
            SELECT COUNT(*)
            FROM entity_mentions
            WHERE entity_mentions.entity_id = entities.id
        )
        "#,
    )
    .execute(&mut **tx)
    .await?;

    sqlx::query("DELETE FROM entities WHERE mention_count = 0")
        .execute(&mut **tx)
        .await?;

    Ok(())
}
