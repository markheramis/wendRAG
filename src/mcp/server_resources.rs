/**
 * MCP resource handler methods for the WendRagServer, building JSON payloads
 * for the `rag://status`, `rag://documents`, `rag://documents/{id}`, and
 * `rag://config` resources.
 */

use rmcp::ErrorData;
use rmcp::model::{ReadResourceResult, ResourceContents};

use super::server::{
    CONFIG_RESOURCE_URI, DOCUMENTS_RESOURCE_URI, DOCUMENT_DETAIL_URI_PREFIX, STATUS_RESOURCE_URI,
    WendRagServer,
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
}
