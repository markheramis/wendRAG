/**
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
    graph: &DocumentEntityGraph,
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

    for entity in &graph.entities {
        let new_id = Uuid::new_v4().to_string();
        let embedding_blob = entity
            .embedding
            .is_empty()
            .then(|| None)
            .unwrap_or_else(|| Some(embedding_to_blob(&entity.embedding)));

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

    for mention in &graph.mentions {
        let Some(chunk_id) = chunk_ids_by_index.get(&mention.chunk_index) else {
            continue;
        };
        let Some(entity_id) = entity_ids_by_key
            .get(&(mention.normalized_name.clone(), mention.entity_type.clone()))
        else {
            continue;
        };

        sqlx::query(
            r#"
            INSERT INTO entity_mentions (chunk_id, entity_id)
            VALUES (?, ?)
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(chunk_id)
        .bind(entity_id)
        .execute(&mut *tx)
        .await?;
    }

    for relationship in &graph.relationships {
        let Some(source_entity_id) = entity_ids_by_key.get(&(
            relationship.source_normalized_name.clone(),
            relationship.source_type.clone(),
        )) else {
            continue;
        };
        let Some(target_entity_id) = entity_ids_by_key.get(&(
            relationship.target_normalized_name.clone(),
            relationship.target_type.clone(),
        )) else {
            continue;
        };
        let evidence_chunk_id = chunk_ids_by_index
            .get(&relationship.evidence_chunk_index)
            .cloned();

        sqlx::query(
            r#"
            INSERT INTO entity_relationships (
                id,
                source_entity_id,
                target_entity_id,
                relationship_type,
                description,
                weight,
                evidence_chunk_id,
                created_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(Uuid::new_v4().to_string())
        .bind(source_entity_id)
        .bind(target_entity_id)
        .bind(&relationship.relationship_type)
        .bind(relationship.description.as_deref())
        .bind(relationship.weight as f64)
        .bind(evidence_chunk_id)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
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
