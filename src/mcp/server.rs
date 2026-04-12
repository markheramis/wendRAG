/**
 * MCP server core: struct definitions, tool-router implementation, and
 * ServerHandler trait implementation. Resource handlers and document
 * reconstruction helpers are delegated to sibling modules.
 */

use std::collections::HashMap;
use std::sync::Arc;

use rmcp::ErrorData;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    Annotated, ListResourcesResult, ListResourceTemplatesResult, PaginatedRequestParams,
    RawResource, RawResourceTemplate, ReadResourceRequestParams, ReadResourceResult,
    ServerCapabilities, ServerInfo,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::config::ChunkingStrategy;
use crate::embed::EmbeddingProvider;
use crate::entity::{EntityExtractor, GraphSettings};
use crate::ingest::pipeline;
use crate::ingest::pipeline::{ContentIngestRequest, DirectoryIngestRequest, IngestOptions};
use crate::ingest::reader::detect_file_type;
use crate::rerank::RerankerProvider;
use crate::retrieve::{ScoredChunk, SearchMode};
use crate::retrieve::{dense, hybrid, sparse};
use crate::store::{SearchFilters, StorageBackend};

use super::reconstruct::{json_err, reconstruct_document_from_chunks};
use super::tools::*;

/// Maximum number of documents ingested concurrently in batch flows.
const MAX_CONCURRENT_BATCH_INGESTS: usize = 4;

pub(super) const STATUS_RESOURCE_URI: &str = "rag://status";
pub(super) const DOCUMENTS_RESOURCE_URI: &str = "rag://documents";
pub(super) const CONFIG_RESOURCE_URI: &str = "rag://config";
pub(super) const DOCUMENT_DETAIL_URI_TEMPLATE: &str = "rag://documents/{id}";
pub(super) const DOCUMENT_DETAIL_URI_PREFIX: &str = "rag://documents/";

/// Display-safe server configuration exposed through the `rag://config` resource.
/// API keys and secrets are intentionally omitted.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub storage_backend: String,
    pub embedding_provider: String,
    pub embedding_model: String,
    pub embedding_dimensions: usize,
    pub entity_extraction_enabled: bool,
    pub graph_retrieval_enabled: bool,
    pub graph_traversal_depth: u8,
    pub chunking_strategy: String,
    pub chunking_semantic_threshold: f64,
    pub reranker_enabled: bool,
    pub reranker_provider: String,
    pub reranker_model: String,
}

#[derive(Clone)]
pub struct WendRagServer {
    pub(super) storage: Arc<dyn StorageBackend>,
    pub(super) embedder: Arc<dyn EmbeddingProvider>,
    entity_extractor: Option<Arc<dyn EntityExtractor>>,
    reranker: Option<Arc<dyn RerankerProvider>>,
    /// Number of candidates to pass to the reranker before trimming.
    reranker_top_n: usize,
    graph_settings: GraphSettings,
    chunking_strategy: ChunkingStrategy,
    semantic_threshold: f64,
    pub(super) server_config: ServerConfig,
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Clone)]
struct MatchedDocumentCandidate {
    file_path: String,
    file_name: String,
    score: f64,
    matched_chunk_indices: Vec<i32>,
}

#[tool_router]
impl WendRagServer {
    pub fn new(
        storage: Arc<dyn StorageBackend>,
        embedder: Arc<dyn EmbeddingProvider>,
        entity_extractor: Option<Arc<dyn EntityExtractor>>,
        reranker: Option<Arc<dyn RerankerProvider>>,
        reranker_top_n: usize,
        graph_settings: GraphSettings,
        chunking_strategy: ChunkingStrategy,
        semantic_threshold: f64,
        server_config: ServerConfig,
    ) -> Self {
        Self {
            storage,
            embedder,
            entity_extractor,
            reranker,
            reranker_top_n,
            graph_settings,
            chunking_strategy,
            semantic_threshold,
            server_config,
            tool_router: Self::tool_router(),
        }
    }

    /**
     * Executes the shared retrieval flow used by both context MCP tools and
     * applies the caller-provided score threshold before serialization.
     */
    /**
     * Executes the shared retrieval flow used by both context MCP tools.
     *
     * When a reranker is configured, the retrieval stage over-fetches
     * `reranker_top_n` candidates, applies the optional score threshold,
     * then reranks the survivors down to the caller's `top_k`. When no
     * reranker is present the pipeline behaves exactly as before.
     */
    async fn search_context_chunks(
        &self,
        input: &SearchInput,
    ) -> Result<(SearchMode, Vec<ScoredChunk>), String> {
        let mode = input
            .mode
            .as_deref()
            .map(SearchMode::from_str_loose)
            .unwrap_or(SearchMode::Hybrid);
        let requested_top_k = input.top_k.unwrap_or(10) as usize;

        // Over-fetch when reranking is enabled so the reranker has enough
        // candidates to choose from.
        let retrieval_top_k = if self.reranker.is_some() {
            self.reranker_top_n.max(requested_top_k) as i64
        } else {
            requested_top_k as i64
        };

        let filters = SearchFilters {
            project: input.project.clone(),
            file_types: input.file_types.clone(),
            tags: input.tags.clone(),
        };

        let results: Vec<ScoredChunk> = match mode {
            SearchMode::Dense => {
                let emb = match self
                    .embedder
                    .embed(std::slice::from_ref(&input.query))
                    .await
                {
                    Ok(mut v) if !v.is_empty() => v.remove(0),
                    Ok(_) => return Err("embedding returned empty".to_string()),
                    Err(error) => return Err(error.to_string()),
                };
                dense::search(&self.storage, &emb, retrieval_top_k, &filters)
                    .await
                    .map_err(|error| error.to_string())?
            }
            SearchMode::Sparse => {
                sparse::search(&self.storage, &input.query, retrieval_top_k, &filters)
                    .await
                    .map_err(|error| error.to_string())?
            }
            SearchMode::Hybrid => hybrid::search(
                &self.storage,
                &self.embedder,
                &input.query,
                retrieval_top_k,
                &filters,
                self.graph_settings,
            )
            .await
            .map_err(|error| error.to_string())?,
        };

        // Apply caller-provided score threshold.
        let mut filtered: Vec<ScoredChunk> = results
            .into_iter()
            .filter(|chunk| {
                input
                    .threshold
                    .is_none_or(|threshold| chunk.score >= threshold)
            })
            .collect();

        // Rerank if a provider is configured.
        if let Some(reranker) = &self.reranker {
            let documents: Vec<String> = filtered.iter().map(|c| c.content.clone()).collect();

            if !documents.is_empty() {
                match reranker
                    .rerank(&input.query, &documents, requested_top_k)
                    .await
                {
                    Ok(reranked) => {
                        let mut reordered: Vec<ScoredChunk> = reranked
                            .into_iter()
                            .filter_map(|r| {
                                filtered.get(r.index).map(|chunk| {
                                    let mut reranked_chunk = chunk.clone();
                                    reranked_chunk.score = r.relevance_score;
                                    reranked_chunk
                                })
                            })
                            .collect();
                        reordered.truncate(requested_top_k);
                        filtered = reordered;
                    }
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            "reranker failed, falling back to original ranking"
                        );
                        // Graceful degradation: keep the original order but
                        // trim to the requested top_k.
                        filtered.truncate(requested_top_k);
                    }
                }
            }
        } else {
            filtered.truncate(requested_top_k);
        }

        Ok((mode, filtered))
    }

    /**
     * Collapses chunk-level search hits into unique matched documents and
     * reconstructs each document body from its stored ordered chunks.
     */
    async fn build_full_context_results(
        &self,
        chunks: Vec<ScoredChunk>,
    ) -> Result<Vec<FullContextResultItem>, String> {
        let mut document_positions: HashMap<String, usize> = HashMap::new();
        let mut documents: Vec<MatchedDocumentCandidate> = Vec::new();

        for chunk in chunks {
            if let Some(position) = document_positions.get(&chunk.file_path).copied() {
                let document = &mut documents[position];
                document.score = document.score.max(chunk.score);
                if !document.matched_chunk_indices.contains(&chunk.chunk_index) {
                    document.matched_chunk_indices.push(chunk.chunk_index);
                }
                continue;
            }

            document_positions.insert(chunk.file_path.clone(), documents.len());
            documents.push(MatchedDocumentCandidate {
                file_path: chunk.file_path,
                file_name: chunk.file_name,
                score: chunk.score,
                matched_chunk_indices: vec![chunk.chunk_index],
            });
        }

        let mut results: Vec<FullContextResultItem> = Vec::with_capacity(documents.len());

        for mut document in documents {
            document.matched_chunk_indices.sort_unstable();
            let chunks = self
                .storage
                .get_document_chunks(&document.file_path)
                .await
                .map_err(|error| error.to_string())?;

            if chunks.is_empty() {
                return Err(format!(
                    "no stored chunks found for matched document: {}",
                    document.file_path
                ));
            }

            results.push(FullContextResultItem {
                document_content: reconstruct_document_from_chunks(&chunks),
                file_path: document.file_path,
                file_name: document.file_name,
                score: document.score,
                matched_chunk_indices: document.matched_chunk_indices,
            });
        }

        Ok(results)
    }

    #[tool(
        description = "Ingest a single local file or HTTP(S) URL into the RAG knowledge base. Provide either a server-accessible file_path/URL OR inline content with a file_name."
    )]
    async fn rag_ingest(&self, Parameters(input): Parameters<IngestInput>) -> String {
        let tags = input.tags.unwrap_or_default();
        let project = input.project.as_deref();

        let result = if let Some(content) = input.content {
            let file_name = input.file_name.unwrap_or_else(|| "unnamed.txt".into());
            let file_type = detect_file_type(&file_name).unwrap_or("text");
            pipeline::ingest_content(
                &self.storage,
                &self.embedder,
                ContentIngestRequest {
                    file_path: &file_name,
                    file_name: &file_name,
                    file_type,
                    text: &content,
                },
                &IngestOptions::new(
                    project,
                    &tags,
                    self.entity_extractor.as_ref(),
                    self.chunking_strategy,
                    self.semantic_threshold,
                ),
            )
            .await
        } else if let Some(file_path) = input.file_path {
            pipeline::ingest_file(
                &self.storage,
                &self.embedder,
                self.entity_extractor.as_ref(),
                &file_path,
                project,
                &tags,
                self.chunking_strategy,
                self.semantic_threshold,
            )
            .await
        } else {
            return json_err("Provide either file_path or content");
        };

        match result {
            Ok(r) => serde_json::to_string(&IngestOutput {
                document_id: r.document_id.to_string(),
                file_path: r.file_path,
                chunk_count: r.chunk_count,
                status: r.status.to_string(),
            })
            .unwrap_or_else(|e| json_err(&e.to_string())),
            Err(e) => json_err(&e.to_string()),
        }
    }

    #[tool(
        description = "Ingest all supported local files (markdown, text, PDF) from a directory into the RAG knowledge base."
    )]
    async fn rag_ingest_directory(
        &self,
        Parameters(input): Parameters<IngestDirectoryInput>,
    ) -> String {
        let tags = input.tags.unwrap_or_default();
        let project = input.project.as_deref();
        let recursive = input.recursive.unwrap_or(true);
        let delete_removed = input.delete_removed.unwrap_or(false);

        match pipeline::ingest_directory(
            &self.storage,
            &self.embedder,
            DirectoryIngestRequest {
                directory_path: &input.directory_path,
                recursive,
                glob_pattern: input.glob.as_deref(),
                delete_removed,
            },
            &IngestOptions::new(
                project,
                &tags,
                self.entity_extractor.as_ref(),
                self.chunking_strategy,
                self.semantic_threshold,
            ),
        )
        .await
        {
            Ok(output) => {
                serde_json::to_string(&output).unwrap_or_else(|e| json_err(&e.to_string()))
            }
            Err(error) => json_err(&error.to_string()),
        }
    }

    /**
     * Batch-ingest documents by inline content. The client reads files locally
     * and sends their content over the wire — works even when the server runs
     * remotely and cannot access the client filesystem.
     *
     * Up to [`MAX_CONCURRENT_BATCH_INGESTS`] documents are processed in
     * parallel. Results are returned in the original input order.
     */
    #[tool(
        description = "Ingest a batch of documents by inline content. The client reads files locally and sends content to the server. Use this for remote deployments where the server cannot access the client filesystem."
    )]
    async fn rag_ingest_batch(&self, Parameters(input): Parameters<IngestBatchInput>) -> String {
        let tags: Vec<String> = input.tags.unwrap_or_default();
        let project: Option<String> = input.project;

        let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_BATCH_INGESTS));
        let mut tasks: JoinSet<(usize, String, Result<String, String>)> = JoinSet::new();

        for (index, item) in input.documents.into_iter().enumerate() {
            let file_type_str = match detect_file_type(&item.file_name) {
                Some(ft) => ft.to_string(),
                None => {
                    let file_name = item.file_name.clone();
                    tasks.spawn(async move {
                        (
                            index,
                            file_name,
                            Err("error: unsupported file type".to_string()),
                        )
                    });
                    continue;
                }
            };

            let storage = self.storage.clone();
            let embedder = self.embedder.clone();
            let entity_extractor = self.entity_extractor.clone();
            let tags = tags.clone();
            let project = project.clone();
            let chunking_strategy = self.chunking_strategy;
            let semantic_threshold = self.semantic_threshold;
            let permit = semaphore.clone().acquire_owned().await.unwrap();

            tasks.spawn(async move {
                let result = pipeline::ingest_content(
                    &storage,
                    &embedder,
                    ContentIngestRequest {
                        file_path: &item.file_name,
                        file_name: &item.file_name,
                        file_type: &file_type_str,
                        text: &item.content,
                    },
                    &IngestOptions::new(
                        project.as_deref(),
                        &tags,
                        entity_extractor.as_ref(),
                        chunking_strategy,
                        semantic_threshold,
                    ),
                )
                .await;
                drop(permit);

                let status = match &result {
                    Ok(r) => Ok(r.status.to_string()),
                    Err(e) => Err(format!("error: {e}")),
                };
                (index, item.file_name, status)
            });
        }

        let mut indexed_results: Vec<(usize, String, Result<String, String>)> = Vec::new();

        while let Some(join_result) = tasks.join_next().await {
            match join_result {
                Ok(tuple) => indexed_results.push(tuple),
                Err(join_error) => {
                    tracing::error!("batch ingestion task panicked: {join_error}");
                }
            }
        }

        indexed_results.sort_by_key(|(index, _, _)| *index);

        let mut output = IngestDirectoryOutput {
            added: 0,
            updated: 0,
            unchanged: 0,
            deleted: 0,
            failed: 0,
            documents: Vec::with_capacity(indexed_results.len()),
        };

        for (_index, file_name, result) in indexed_results {
            match result {
                Ok(status) => {
                    match status.as_str() {
                        "created" => output.added += 1,
                        "updated" => output.updated += 1,
                        _ => output.unchanged += 1,
                    }
                    output.documents.push(IngestDocStatus {
                        file_path: file_name,
                        status,
                    });
                }
                Err(status) => {
                    output.failed += 1;
                    output.documents.push(IngestDocStatus {
                        file_path: file_name,
                        status,
                    });
                }
            }
        }

        serde_json::to_string(&output).unwrap_or_else(|e| json_err(&e.to_string()))
    }

    #[tool(
        description = "Search the RAG knowledge base and return chunk-level context using hybrid (dense + sparse), dense-only, or sparse-only retrieval."
    )]
    async fn rag_get_context(&self, Parameters(input): Parameters<SearchInput>) -> String {
        let (mode, results) = match self.search_context_chunks(&input).await {
            Ok(results) => results,
            Err(error) => return json_err(&error),
        };

        let output = SearchOutput {
            mode: mode.as_str().to_string(),
            results: results
                .into_iter()
                .map(|chunk| SearchResultItem {
                    chunk_content: chunk.content,
                    section_title: chunk.section_title,
                    file_path: chunk.file_path,
                    file_name: chunk.file_name,
                    score: chunk.score,
                    chunk_index: chunk.chunk_index,
                })
                .collect(),
        };

        serde_json::to_string(&output).unwrap_or_else(|error| json_err(&error.to_string()))
    }

    #[tool(
        description = "Search the RAG knowledge base and return reconstructed full-document context for each matched document."
    )]
    async fn rag_get_full_context(&self, Parameters(input): Parameters<SearchInput>) -> String {
        let (mode, chunks) = match self.search_context_chunks(&input).await {
            Ok(results) => results,
            Err(error) => return json_err(&error),
        };

        let results = match self.build_full_context_results(chunks).await {
            Ok(results) => results,
            Err(error) => return json_err(&error),
        };

        serde_json::to_string(&FullContextOutput {
            mode: mode.as_str().to_string(),
            results,
        })
        .unwrap_or_else(|error| json_err(&error.to_string()))
    }

    #[tool(
        description = "List all documents indexed in the RAG knowledge base, with optional project and file type filters."
    )]
    async fn rag_list_sources(&self, Parameters(input): Parameters<ListSourcesInput>) -> String {
        match self
            .storage
            .list_documents(input.project.as_deref(), input.file_type.as_deref())
            .await
        {
            Ok(docs) => {
                let output = ListSourcesOutput {
                    documents: docs
                        .into_iter()
                        .map(|d| DocumentInfo {
                            id: d.id.to_string(),
                            file_path: d.file_path,
                            file_name: d.file_name,
                            file_type: d.file_type,
                            project: d.project,
                            tags: d.tags,
                            chunk_count: d.chunk_count,
                            created_at: d.created_at.to_rfc3339(),
                            updated_at: d.updated_at.to_rfc3339(),
                        })
                        .collect(),
                };
                serde_json::to_string(&output).unwrap_or_else(|e| json_err(&e.to_string()))
            }
            Err(e) => json_err(&e.to_string()),
        }
    }

    #[tool(
        description = "Delete a document and all its chunks from the RAG knowledge base. Provide file_path or document_id."
    )]
    async fn rag_delete_source(&self, Parameters(input): Parameters<DeleteSourceInput>) -> String {
        let doc_id = input
            .document_id
            .as_deref()
            .map(|s| s.parse::<uuid::Uuid>())
            .transpose();

        let doc_id = match doc_id {
            Ok(id) => id,
            Err(e) => return json_err(&format!("invalid document_id: {e}")),
        };

        match self
            .storage
            .delete_document(input.file_path.as_deref(), doc_id)
            .await
        {
            Ok(Some((path, count))) => serde_json::to_string(&DeleteSourceOutput {
                deleted: true,
                file_path: Some(path),
                chunk_count_removed: count,
            })
            .unwrap_or_else(|e| json_err(&e.to_string())),
            Ok(None) => serde_json::to_string(&DeleteSourceOutput {
                deleted: false,
                file_path: None,
                chunk_count_removed: 0,
            })
            .unwrap_or_else(|e| json_err(&e.to_string())),
            Err(e) => json_err(&e.to_string()),
        }
    }
}

// ─── MCP server handler ───────────────────────────────────────────────────────

#[tool_handler]
impl ServerHandler for WendRagServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "RAG knowledge base server for markdown, text, PDF, and URL documents. \
             Supports hybrid search with dense (vector) and sparse (full-text + trigram) \
             retrieval fused via Reciprocal Rank Fusion."
                .into(),
        );
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build();
        info
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(vec![
            Annotated::new(
                RawResource::new(STATUS_RESOURCE_URI, "status")
                    .with_title("Server Status")
                    .with_description(
                        "Server health, document count, chunk count, and active configuration.",
                    )
                    .with_mime_type("application/json"),
                None,
            ),
            Annotated::new(
                RawResource::new(DOCUMENTS_RESOURCE_URI, "documents")
                    .with_title("Document List")
                    .with_description(
                        "All indexed documents with metadata and chunk counts.",
                    )
                    .with_mime_type("application/json"),
                None,
            ),
            Annotated::new(
                RawResource::new(CONFIG_RESOURCE_URI, "config")
                    .with_title("Server Configuration")
                    .with_description("Active server configuration (API keys redacted).")
                    .with_mime_type("application/json"),
                None,
            ),
        ]))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(ListResourceTemplatesResult::with_all_items(vec![
            Annotated::new(
                RawResourceTemplate::new(DOCUMENT_DETAIL_URI_TEMPLATE, "document-detail")
                    .with_title("Document Detail")
                    .with_description(
                        "Metadata and chunk previews for a single document. \
                         Replace {id} with a document UUID from rag://documents.",
                    )
                    .with_mime_type("application/json"),
                None,
            ),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        match request.uri.as_str() {
            STATUS_RESOURCE_URI => self.resource_status().await,
            DOCUMENTS_RESOURCE_URI => self.resource_documents().await,
            CONFIG_RESOURCE_URI => self.resource_config(),
            uri if uri.starts_with(DOCUMENT_DETAIL_URI_PREFIX) => {
                let id = uri[DOCUMENT_DETAIL_URI_PREFIX.len()..].to_owned();
                self.resource_document_detail(&id).await
            }
            uri => Err(ErrorData::resource_not_found(
                format!("unknown resource URI: {uri}"),
                None,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;

    use rmcp::model::ResourceContents;

    use super::*;
    use crate::embed::EmbeddingProvider;
    use crate::embed::provider::EmbeddingError;
    use crate::entity::GraphSettings;
    use crate::store::SqliteBackend;

    struct NoopEmbedder;

    #[async_trait]
    impl EmbeddingProvider for NoopEmbedder {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
            Ok(texts.iter().map(|_| vec![0.0f32; 1024]).collect())
        }
    }

    fn test_server_config() -> ServerConfig {
        ServerConfig {
            storage_backend: "sqlite".to_string(),
            embedding_provider: "openai".to_string(),
            embedding_model: "test-model".to_string(),
            embedding_dimensions: 1024,
            entity_extraction_enabled: false,
            graph_retrieval_enabled: false,
            graph_traversal_depth: 2,
            chunking_strategy: "fixed".to_string(),
            chunking_semantic_threshold: 0.25,
            reranker_enabled: false,
            reranker_provider: "none".to_string(),
            reranker_model: String::new(),
        }
    }

    async fn make_test_server() -> (WendRagServer, tempfile::NamedTempFile) {
        let tmpfile = tempfile::NamedTempFile::new().unwrap();
        let storage = SqliteBackend::connect(
            tmpfile.path().to_str().unwrap(),
            &crate::config::PoolConfig::default(),
        )
        .await
        .unwrap();
        let storage: Arc<dyn crate::store::StorageBackend> = Arc::new(storage);
        let embedder: Arc<dyn EmbeddingProvider> = Arc::new(NoopEmbedder);
        let server = WendRagServer::new(
            storage,
            embedder,
            None,
            None,
            30,
            GraphSettings::new(false, 2),
            crate::config::ChunkingStrategy::Fixed,
            0.25,
            test_server_config(),
        );
        (server, tmpfile)
    }

    fn extract_json_text(result: &ReadResourceResult) -> serde_json::Value {
        match &result.contents[0] {
            ResourceContents::TextResourceContents { text, .. } => {
                serde_json::from_str(text).unwrap()
            }
            _ => panic!("expected TextResourceContents"),
        }
    }

    #[tokio::test]
    async fn resource_config_returns_expected_fields() {
        let (server, _tmp) = make_test_server().await;
        let result = server.resource_config().unwrap();

        assert_eq!(result.contents.len(), 1);
        let json = extract_json_text(&result);

        assert_eq!(json["storage_backend"], "sqlite");
        assert_eq!(json["embedding_provider"], "openai");
        assert_eq!(json["embedding_model"], "test-model");
        assert_eq!(json["embedding_dimensions"], 1024);
        assert_eq!(json["entity_extraction_enabled"], false);
        assert_eq!(json["graph_retrieval_enabled"], false);
        assert_eq!(json["graph_traversal_depth"], 2);
        assert_eq!(json["chunking_strategy"], "fixed");
        assert_eq!(json["chunking_semantic_threshold"], 0.25);
    }

    #[tokio::test]
    async fn resource_config_mime_type_is_application_json() {
        let (server, _tmp) = make_test_server().await;
        let result = server.resource_config().unwrap();

        match &result.contents[0] {
            ResourceContents::TextResourceContents { mime_type, uri, .. } => {
                assert_eq!(mime_type.as_deref(), Some("application/json"));
                assert_eq!(uri, CONFIG_RESOURCE_URI);
            }
            _ => panic!("expected TextResourceContents"),
        }
    }

    #[tokio::test]
    async fn resource_status_returns_zero_counts_for_empty_db() {
        let (server, _tmp) = make_test_server().await;
        let result = server.resource_status().await.unwrap();

        let json = extract_json_text(&result);
        assert_eq!(json["status"], "ok");
        assert_eq!(json["document_count"], 0);
        assert_eq!(json["chunk_count"], 0);
        assert_eq!(json["storage_backend"], "sqlite");
    }

    #[tokio::test]
    async fn resource_documents_returns_empty_list_for_fresh_db() {
        let (server, _tmp) = make_test_server().await;
        let result = server.resource_documents().await.unwrap();

        let json = extract_json_text(&result);
        assert_eq!(json["total"], 0);
        assert!(json["documents"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn resource_document_detail_with_invalid_uuid_returns_error() {
        let (server, _tmp) = make_test_server().await;
        let err = server
            .resource_document_detail("not-a-uuid")
            .await
            .unwrap_err();
        assert!(err.message.contains("invalid document id"));
    }

    #[tokio::test]
    async fn resource_document_detail_with_nonexistent_uuid_returns_not_found() {
        let (server, _tmp) = make_test_server().await;
        let id = uuid::Uuid::new_v4().to_string();
        let err = server.resource_document_detail(&id).await.unwrap_err();
        assert!(err.message.contains("document not found"));
    }
}
