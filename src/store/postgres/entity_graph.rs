/*!
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
    graph: &mut DocumentEntityGraph,
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

    // PERF-03: move the owned embedding `Vec<f32>` into the pgvector
    // `Vector` per iteration instead of cloning. A 1024-dim embedding is
    // 4 KiB; the previous `.clone()` caused ~200 KiB of allocation churn
    // per document with 50 entities.
    for entity in &mut graph.entities {
        let embedding = std::mem::take(&mut entity.embedding);
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
        .bind(Vector::from(embedding))
        .fetch_one(&mut *tx)
        .await?;

        entity_ids_by_key.insert(
            (entity.normalized_name.clone(), entity.entity_type.clone()),
            row.0,
        );
    }

    /// Rows per multi-row INSERT batch for mentions and relationships.
    const BATCH_SIZE: usize = 50;

    let resolved_mentions: Vec<(Uuid, Uuid)> = graph
        .mentions
        .iter()
        .filter_map(|mention| {
            let chunk_id = chunk_ids_by_index.get(&mention.chunk_index).copied()?;
            let entity_id = entity_ids_by_key
                .get(&(mention.normalized_name.clone(), mention.entity_type.clone()))
                .copied()?;
            Some((chunk_id, entity_id))
        })
        .collect();

    for batch in resolved_mentions.chunks(BATCH_SIZE) {
        let mut builder = sqlx::QueryBuilder::new(
            "INSERT INTO entity_mentions (chunk_id, entity_id) ",
        );
        builder.push_values(batch, |mut row, (chunk_id, entity_id)| {
            row.push_bind(*chunk_id).push_bind(*entity_id);
        });
        builder.push(" ON CONFLICT DO NOTHING");
        builder.build().execute(&mut *tx).await?;
    }

    struct ResolvedRelationship<'a> {
        source_entity_id: Uuid,
        target_entity_id: Uuid,
        relationship_type: &'a str,
        description: Option<&'a str>,
        weight: f64,
        evidence_chunk_id: Option<Uuid>,
    }

    let resolved_relationships: Vec<ResolvedRelationship<'_>> = graph
        .relationships
        .iter()
        .filter_map(|rel| {
            let source_entity_id = entity_ids_by_key
                .get(&(rel.source_normalized_name.clone(), rel.source_type.clone()))
                .copied()?;
            let target_entity_id = entity_ids_by_key
                .get(&(rel.target_normalized_name.clone(), rel.target_type.clone()))
                .copied()?;
            let evidence_chunk_id = chunk_ids_by_index
                .get(&rel.evidence_chunk_index)
                .copied();
            Some(ResolvedRelationship {
                source_entity_id,
                target_entity_id,
                relationship_type: &rel.relationship_type,
                description: rel.description.as_deref(),
                weight: rel.weight as f64,
                evidence_chunk_id,
            })
        })
        .collect();

    for batch in resolved_relationships.chunks(BATCH_SIZE) {
        let mut builder = sqlx::QueryBuilder::new(
            "INSERT INTO entity_relationships (source_entity_id, target_entity_id, relationship_type, description, weight, evidence_chunk_id) ",
        );
        builder.push_values(batch, |mut row, rel| {
            row.push_bind(rel.source_entity_id)
                .push_bind(rel.target_entity_id)
                .push_bind(rel.relationship_type)
                .push_bind(rel.description)
                .push_bind(rel.weight)
                .push_bind(rel.evidence_chunk_id);
        });
        builder.build().execute(&mut *tx).await?;
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
