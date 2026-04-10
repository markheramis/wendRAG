/**
 * Entity graph persistence and orphan pruning for the PostgreSQL backend.
 */

use std::collections::HashMap;

use pgvector::Vector;
use sqlx::PgPool;
use uuid::Uuid;

use crate::entity::DocumentEntityGraph;

/**
 * Persists the deduplicated entity graph for a document after its chunks
 * have been written.
 */
pub(crate) async fn replace_document_entity_graph(
    pool: &PgPool,
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
    let chunk_rows = sqlx::query_as::<_, (Uuid, i32)>(
        "SELECT id, chunk_index FROM chunks WHERE document_id = $1",
    )
    .bind(document_id)
    .fetch_all(&mut *tx)
    .await?;
    let chunk_ids_by_index: HashMap<i32, Uuid> = chunk_rows
        .into_iter()
        .map(|(id, index)| (index, id))
        .collect();
    let mut entity_ids_by_key: HashMap<(String, String), Uuid> = HashMap::new();

    for entity in &graph.entities {
        let row = sqlx::query_as::<_, (Uuid,)>(
            r#"
            INSERT INTO entities (
                normalized_name,
                name,
                entity_type,
                description,
                embedding,
                mention_count
            )
            VALUES ($1, $2, $3, $4, $5, 0)
            ON CONFLICT (normalized_name, entity_type) DO UPDATE
            SET
                name = EXCLUDED.name,
                description = COALESCE(EXCLUDED.description, entities.description),
                embedding = EXCLUDED.embedding
            RETURNING id
            "#,
        )
        .bind(&entity.normalized_name)
        .bind(&entity.display_name)
        .bind(&entity.entity_type)
        .bind(entity.description.as_deref())
        .bind(Vector::from(entity.embedding.clone()))
        .fetch_one(&mut *tx)
        .await?;

        entity_ids_by_key.insert(
            (entity.normalized_name.clone(), entity.entity_type.clone()),
            row.0,
        );
    }

    for mention in &graph.mentions {
        let Some(chunk_id) = chunk_ids_by_index.get(&mention.chunk_index).copied() else {
            continue;
        };
        let Some(entity_id) = entity_ids_by_key
            .get(&(mention.normalized_name.clone(), mention.entity_type.clone()))
            .copied()
        else {
            continue;
        };

        sqlx::query(
            r#"
            INSERT INTO entity_mentions (chunk_id, entity_id)
            VALUES ($1, $2)
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(chunk_id)
        .bind(entity_id)
        .execute(&mut *tx)
        .await?;
    }

    for relationship in &graph.relationships {
        let Some(source_entity_id) = entity_ids_by_key
            .get(&(
                relationship.source_normalized_name.clone(),
                relationship.source_type.clone(),
            ))
            .copied()
        else {
            continue;
        };
        let Some(target_entity_id) = entity_ids_by_key
            .get(&(
                relationship.target_normalized_name.clone(),
                relationship.target_type.clone(),
            ))
            .copied()
        else {
            continue;
        };
        let evidence_chunk_id = chunk_ids_by_index
            .get(&relationship.evidence_chunk_index)
            .copied();

        sqlx::query(
            r#"
            INSERT INTO entity_relationships (
                source_entity_id,
                target_entity_id,
                relationship_type,
                description,
                weight,
                evidence_chunk_id
            )
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(source_entity_id)
        .bind(target_entity_id)
        .bind(&relationship.relationship_type)
        .bind(relationship.description.as_deref())
        .bind(relationship.weight as f64)
        .bind(evidence_chunk_id)
        .execute(&mut *tx)
        .await?;
    }

    prune_orphan_entities(&mut tx).await?;
    tx.commit().await
}

/**
 * Recomputes mention counts and removes orphaned entities after a document's
 * graph data changes.
 */
pub(crate) async fn prune_orphan_entities(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE entities AS entity
        SET mention_count = counts.mention_count
        FROM (
            SELECT entity_id, COUNT(*)::integer AS mention_count
            FROM entity_mentions
            GROUP BY entity_id
        ) AS counts
        WHERE counts.entity_id = entity.id
        "#,
    )
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        r#"
        UPDATE entities
        SET mention_count = 0
        WHERE id NOT IN (SELECT DISTINCT entity_id FROM entity_mentions)
        "#,
    )
    .execute(&mut **tx)
    .await?;

    sqlx::query("DELETE FROM entities WHERE mention_count = 0")
        .execute(&mut **tx)
        .await?;

    Ok(())
}
