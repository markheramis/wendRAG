/**
 * MCP resource handler methods for the WendRagServer, building JSON payloads
 * for the `rag://status`, `rag://documents`, `rag://documents/{id}`,
 * `rag://config`, and `rag://communities` resources.
 */

use rmcp::ErrorData;
use rmcp::model::{ReadResourceResult, ResourceContents};

use super::server::{
    COMMUNITIES_RESOURCE_URI, CONFIG_RESOURCE_URI, DOCUMENTS_RESOURCE_URI,
    DOCUMENT_DETAIL_URI_PREFIX, MEMORY_STATUS_RESOURCE_URI, STATUS_RESOURCE_URI, WendRagServer,
};

impl WendRagServer {
    /**
     * Builds the `rag://status` resource payload: live document/chunk counts
     * plus the active storage and retrieval configuration.
     */
    pub(super) async fn resource_status(&self) -> Result<ReadResourceResult, ErrorData> {
        let (doc_count, chunk_count) = self
            .storage
            .count_documents_and_chunks()
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let payload = serde_json::json!({
            "status": "ok",
            "document_count": doc_count,
            "chunk_count": chunk_count,
            "storage_backend": self.server_config.storage_backend,
            "entity_extraction_enabled": self.server_config.entity_extraction_enabled,
            "graph_retrieval_enabled": self.server_config.graph_retrieval_enabled,
            "graph_traversal_depth": self.server_config.graph_traversal_depth,
            "embedding_model": self.server_config.embedding_model,
            "embedding_provider": self.server_config.embedding_provider,
        });

        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            payload.to_string(),
            STATUS_RESOURCE_URI,
        )
        .with_mime_type("application/json")]))
    }

    /**
     * Builds the `rag://documents` resource payload: the full document list
     * with metadata and chunk counts, unfiltered.
     */
    pub(super) async fn resource_documents(&self) -> Result<ReadResourceResult, ErrorData> {
        let docs = self
            .storage
            .list_documents(None, None)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let payload = serde_json::json!({
            "total": docs.len(),
            "documents": docs.iter().map(|d| serde_json::json!({
                "id": d.id.to_string(),
                "file_path": d.file_path,
                "file_name": d.file_name,
                "file_type": d.file_type,
                "project": d.project,
                "tags": d.tags,
                "chunk_count": d.chunk_count,
                "created_at": d.created_at.to_rfc3339(),
                "updated_at": d.updated_at.to_rfc3339(),
            })).collect::<Vec<_>>(),
        });

        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            payload.to_string(),
            DOCUMENTS_RESOURCE_URI,
        )
        .with_mime_type("application/json")]))
    }

    /**
     * Builds a `rag://documents/{id}` resource payload: full document metadata
     * plus chunk-level previews (first 200 chars per chunk).
     */
    pub(super) async fn resource_document_detail(
        &self,
        id: &str,
    ) -> Result<ReadResourceResult, ErrorData> {
        let uri = format!("{DOCUMENT_DETAIL_URI_PREFIX}{id}");

        let doc_id = uuid::Uuid::parse_str(id)
            .map_err(|_| ErrorData::invalid_params(format!("invalid document id: {id}"), None))?;

        let docs = self
            .storage
            .list_documents(None, None)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let doc = docs
            .into_iter()
            .find(|d| d.id == doc_id)
            .ok_or_else(|| {
                ErrorData::resource_not_found(format!("document not found: {id}"), None)
            })?;

        let chunks = self
            .storage
            .get_document_chunks(&doc.file_path)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let payload = serde_json::json!({
            "document": {
                "id": doc.id.to_string(),
                "file_path": doc.file_path,
                "file_name": doc.file_name,
                "file_type": doc.file_type,
                "project": doc.project,
                "tags": doc.tags,
                "chunk_count": doc.chunk_count,
                "created_at": doc.created_at.to_rfc3339(),
                "updated_at": doc.updated_at.to_rfc3339(),
            },
            "chunks": chunks.iter().map(|c| serde_json::json!({
                "chunk_index": c.chunk_index,
                "section_title": c.section_title,
                "content_preview": c.content.chars().take(200).collect::<String>(),
            })).collect::<Vec<_>>(),
        });

        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            payload.to_string(),
            uri,
        )
        .with_mime_type("application/json")]))
    }

    /**
     * Builds the `rag://config` resource payload from the display-safe
     * server configuration (API keys are never included).
     */
    pub(super) fn resource_config(&self) -> Result<ReadResourceResult, ErrorData> {
        let payload = serde_json::json!({
            "storage_backend": self.server_config.storage_backend,
            "embedding_provider": self.server_config.embedding_provider,
            "embedding_model": self.server_config.embedding_model,
            "embedding_dimensions": self.server_config.embedding_dimensions,
            "entity_extraction_enabled": self.server_config.entity_extraction_enabled,
            "graph_retrieval_enabled": self.server_config.graph_retrieval_enabled,
            "graph_traversal_depth": self.server_config.graph_traversal_depth,
            "chunking_strategy": self.server_config.chunking_strategy,
            "chunking_semantic_threshold": self.server_config.chunking_semantic_threshold,
            "chunking_max_sentences": self.server_config.chunking_max_sentences,
            "chunking_filter_garbage": self.server_config.chunking_filter_garbage,
            "reranker_enabled": self.server_config.reranker_enabled,
            "reranker_provider": self.server_config.reranker_provider,
            "reranker_model": self.server_config.reranker_model,
        });

        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            payload.to_string(),
            CONFIG_RESOURCE_URI,
        )
        .with_mime_type("application/json")]))
    }

    /**
     * Builds the `rag://communities` resource payload: all detected entity
     * communities with their names, summaries, importance, and entity counts.
     */
    pub(super) async fn resource_communities(&self) -> Result<ReadResourceResult, ErrorData> {
        let communities = self
            .storage
            .list_communities(None)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let payload = serde_json::json!({
            "total": communities.len(),
            "communities": communities.iter().map(|c| serde_json::json!({
                "id": c.id.to_string(),
                "name": c.name,
                "summary": c.summary,
                "project": c.project,
                "importance": c.importance,
                "entity_count": c.entity_count,
            })).collect::<Vec<_>>(),
        });

        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            payload.to_string(),
            COMMUNITIES_RESOURCE_URI,
        )
        .with_mime_type("application/json")]))
    }

    /**
     * Builds the `rag://memory/status` resource: active sessions, memory
     * counts by scope, and the behavioral protocol that agents should follow.
     */
    pub(super) async fn resource_memory_status(&self) -> Result<ReadResourceResult, ErrorData> {
        let (_enabled, stats_json) = if let Some(mm) = &self.memory_manager {
            let stats = mm.get_stats().await;
            let by_scope: serde_json::Value = stats
                .memories_by_scope
                .iter()
                .fold(serde_json::json!({}), |mut acc, (scope, count)| {
                    acc[scope] = serde_json::json!(count);
                    acc
                });
            (
                true,
                serde_json::json!({
                    "enabled": true,
                    "active_sessions": stats.active_sessions,
                    "total_memories": stats.total_memories,
                    "memories_by_scope": by_scope,
                    "protocol": "Search memory before answering questions about past interactions. \
                                 Store important facts, preferences, and decisions. \
                                 Invalidate stale facts when corrections are provided.",
                }),
            )
        } else {
            (false, serde_json::json!({ "enabled": false }))
        };

        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            stats_json.to_string(),
            MEMORY_STATUS_RESOURCE_URI,
        )
        .with_mime_type("application/json")]))
    }
}
