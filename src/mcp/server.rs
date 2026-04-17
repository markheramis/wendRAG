/*!
 * MCP server core: struct definitions, tool-router implementation, and
 * ServerHandler trait implementation. Resource handlers live in the
 * sibling `server_resources` module.
 */

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

use crate::config::{ChunkingStrategy, CommunityConfig};
use crate::embed::EmbeddingProvider;
use crate::entity::{EntityExtractor, GraphSettings};
use crate::ingest::pipeline;
use crate::ingest::pipeline::{ContentIngestRequest, DirectoryIngestRequest, IngestOptions};
use crate::ingest::reader::detect_file_type;
use crate::memory::manager::MemoryManager;
use crate::memory::types::{MemoryScope, MemoryType};
use crate::rerank::RerankerProvider;
use crate::retrieve::router::{QueryRouter, QueryRouterConfig, QueryScope};
use crate::retrieve::{ScoredChunk, SearchMode};
use crate::retrieve::{dense, hybrid, sparse};
use crate::store::{SearchFilters, StorageBackend};

use super::tools::*;

/**
 * Formats an error message as the JSON payload every MCP tool returns on
 * failure (`{"error": "..."}`). Keeping the shape uniform lets clients
 * display or parse errors without branching on the tool name.
 */
pub(super) fn json_err(msg: &str) -> String {
    serde_json::json!({ "error": msg }).to_string()
}

/// Maximum number of documents ingested concurrently in batch flows.
const MAX_CONCURRENT_BATCH_INGESTS: usize = 4;

pub(super) const STATUS_RESOURCE_URI: &str = "rag://status";
pub(super) const DOCUMENTS_RESOURCE_URI: &str = "rag://documents";
pub(super) const CONFIG_RESOURCE_URI: &str = "rag://config";
pub(super) const COMMUNITIES_RESOURCE_URI: &str = "rag://communities";
pub(super) const MEMORY_STATUS_RESOURCE_URI: &str = "rag://memory/status";
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
    pub chunking_max_sentences: usize,
    pub chunking_filter_garbage: bool,
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
    reranker_top_n: usize,
    graph_settings: GraphSettings,
    query_router: Arc<QueryRouter>,
    community_config: CommunityConfig,
    pub(super) memory_manager: Option<Arc<MemoryManager>>,
    chunking_strategy: ChunkingStrategy,
    semantic_threshold: f64,
    chunking_max_sentences: usize,
    chunking_filter_garbage: bool,
    pub(super) server_config: ServerConfig,
    // Populated at construction and consumed by the `#[tool_handler]`
    // macro's generated dispatch code. rustc's dead-code lint cannot see
    // through the macro expansion, so the field is intentionally tagged
    // as potentially unused.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl WendRagServer {
    /// Construct a fully wired `WendRagServer`. The parameter list mirrors
    /// the resolved runtime configuration from `main.rs`; a builder
    /// indirection would not reduce the total fan-in.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        storage: Arc<dyn StorageBackend>,
        embedder: Arc<dyn EmbeddingProvider>,
        entity_extractor: Option<Arc<dyn EntityExtractor>>,
        reranker: Option<Arc<dyn RerankerProvider>>,
        reranker_top_n: usize,
        graph_settings: GraphSettings,
        community_config: CommunityConfig,
        memory_manager: Option<Arc<MemoryManager>>,
        chunking_strategy: ChunkingStrategy,
        semantic_threshold: f64,
        chunking_max_sentences: usize,
        chunking_filter_garbage: bool,
        server_config: ServerConfig,
    ) -> Self {
        Self {
            storage,
            embedder,
            entity_extractor,
            reranker,
            reranker_top_n,
            graph_settings,
            query_router: Arc::new(QueryRouter::new(QueryRouterConfig::default())),
            community_config,
            memory_manager,
            chunking_strategy,
            semantic_threshold,
            chunking_max_sentences,
            chunking_filter_garbage,
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
        validate_query_size(&input.query)?;
        let explicit_mode = input.mode.as_deref().map(SearchMode::from_str_loose);
        let mode = explicit_mode.unwrap_or(SearchMode::Hybrid);
        let requested_top_k = input.top_k.unwrap_or(10) as usize;

        if explicit_mode.is_none() && self.graph_settings.enabled {
            let classification = self.query_router.classify(&input.query);
            tracing::debug!(
                scope = ?classification.scope,
                confidence = classification.confidence.score,
                "query router classification"
            );
            if classification.scope == QueryScope::Local {
                tracing::debug!("local query detected, community branch will be skipped by graph settings");
            }
        }

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

    #[tool(
        description = "Ingest a single local file or HTTP(S) URL into the RAG knowledge base. Provide either a server-accessible file_path/URL OR inline content with a file_name."
    )]
    async fn rag_ingest(&self, Parameters(input): Parameters<IngestInput>) -> String {
        if let Some(ref content) = input.content
            && let Err(e) = validate_content_size(content, "content")
        {
            return json_err(&e);
        }
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
                    Some(self.community_config.clone()),
                    self.chunking_strategy,
                    self.semantic_threshold,
                    self.chunking_max_sentences,
                    self.chunking_filter_garbage,
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
                self.chunking_max_sentences,
                self.chunking_filter_garbage,
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
                Some(self.community_config.clone()),
                self.chunking_strategy,
                self.semantic_threshold,
                self.chunking_max_sentences,
                self.chunking_filter_garbage,
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
        if let Err(e) = validate_batch_size(input.documents.len()) {
            return json_err(&e);
        }
        for (idx, item) in input.documents.iter().enumerate() {
            if let Err(e) =
                validate_content_size(&item.content, &format!("documents[{idx}].content"))
            {
                return json_err(&e);
            }
        }

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
            let community_config = self.community_config.clone();
            let tags = tags.clone();
            let project = project.clone();
            let chunking_strategy = self.chunking_strategy;
            let semantic_threshold = self.semantic_threshold;
            let chunking_max_sentences = self.chunking_max_sentences;
            let chunking_filter_garbage = self.chunking_filter_garbage;
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
                        Some(community_config.clone()),
                        chunking_strategy,
                        semantic_threshold,
                        chunking_max_sentences,
                        chunking_filter_garbage,
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
        description = "Fetch a specific stored chunk by chunk_index within a document, optionally with before/after neighbours. Useful for reconstructing content that was split across chunks (e.g. a Mermaid diagram code block) after seeing a partial match in rag_get_context."
    )]
    async fn rag_get_chunk(&self, Parameters(input): Parameters<GetChunkInput>) -> String {
        // Selector sanity: exactly one of file_path / document_id must be
        // supplied so the query is unambiguous.
        let (has_path, has_id) = (input.file_path.is_some(), input.document_id.is_some());
        if has_path == has_id {
            return json_err("Provide exactly one of file_path or document_id");
        }
        if input.chunk_index < 0 {
            return json_err("chunk_index must be zero or positive");
        }

        let document_id = match input.document_id.as_deref() {
            Some(id) => match uuid::Uuid::parse_str(id) {
                Ok(uuid) => Some(uuid),
                Err(error) => return json_err(&format!("invalid document_id: {error}")),
            },
            None => None,
        };

        // Cap the neighbourhood size so a single call cannot produce an
        // unbounded response. Saturating subtraction prevents negative
        // ranges when chunk_index is near 0.
        let before = input.before.unwrap_or(0).min(MAX_CHUNK_CONTEXT);
        let after = input.after.unwrap_or(0).min(MAX_CHUNK_CONTEXT);
        let start_index = (input.chunk_index as i64 - before as i64).max(0) as i32;
        let end_index = (input.chunk_index as i64 + after as i64).min(i32::MAX as i64) as i32;

        let rows = match self
            .storage
            .get_chunks_by_index(
                input.file_path.as_deref(),
                document_id,
                start_index,
                end_index,
            )
            .await
        {
            Ok(rows) => rows,
            Err(error) => return json_err(&error.to_string()),
        };

        let output = GetChunkOutput {
            chunks: rows
                .into_iter()
                .map(|row| ChunkItem {
                    document_id: row.document_id.to_string(),
                    file_path: row.file_path,
                    file_name: row.file_name,
                    chunk_index: row.chunk_index,
                    section_title: row.section_title,
                    content: row.content,
                })
                .collect(),
        };

        serde_json::to_string(&output).unwrap_or_else(|error| json_err(&error.to_string()))
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

    #[tool(description = "Store a memory entry (fact, preference, event, or summary) for later retrieval.")]
    async fn memory_store(&self, Parameters(input): Parameters<MemoryStoreInput>) -> String {
        if let Err(e) = validate_content_size(&input.content, "content") {
            return json_err(&e);
        }
        let Some(mm) = &self.memory_manager else {
            return json_err("memory subsystem is not enabled");
        };

        let scope = match input.scope.as_deref() {
            Some("session") => MemoryScope::Session,
            Some("global") => MemoryScope::Global,
            _ => MemoryScope::User,
        };
        let entry_type = match input.entry_type.as_deref() {
            Some("preference") => MemoryType::Preference,
            Some("event") => MemoryType::Event,
            Some("summary") => MemoryType::Summary,
            Some("message") => MemoryType::Message,
            _ => MemoryType::Fact,
        };
        let importance = input.importance.unwrap_or(0.5);

        match mm
            .store_memory(scope, input.session_id, input.user_id, input.content, entry_type, importance)
            .await
        {
            Ok(entry) => serde_json::to_string(&MemoryStoreOutput {
                memory_id: entry.id.to_string(),
                scope: entry.scope.as_str().to_string(),
                entry_type: entry.metadata.entry_type.as_str().to_string(),
            })
            .unwrap_or_else(|e| json_err(&e.to_string())),
            Err(e) => json_err(&e.to_string()),
        }
    }

    #[tool(description = "Search stored memories using semantic similarity. Returns relevant facts, preferences, and past interactions.")]
    async fn memory_retrieve(&self, Parameters(input): Parameters<MemoryRetrieveInput>) -> String {
        if let Err(e) = validate_query_size(&input.query) {
            return json_err(&e);
        }
        let Some(mm) = &self.memory_manager else {
            return json_err("memory subsystem is not enabled");
        };

        let limit = input.limit.unwrap_or(10) as usize;
        match mm.retrieve_memories(&input.query, input.user_id.as_deref(), Some(limit)).await {
            Ok(entries) => {
                let items: Vec<MemoryItem> = entries
                    .iter()
                    .map(|e| MemoryItem {
                        id: e.id.to_string(),
                        content: e.content.clone(),
                        scope: e.scope.as_str().to_string(),
                        entry_type: e.metadata.entry_type.as_str().to_string(),
                        importance: e.importance_score,
                        created_at: e.created_at.to_rfc3339(),
                    })
                    .collect();
                serde_json::to_string(&MemoryRetrieveOutput { memories: items })
                    .unwrap_or_else(|e| json_err(&e.to_string()))
            }
            Err(e) => json_err(&e.to_string()),
        }
    }

    #[tool(description = "Forget (delete or invalidate) a memory entry by its ID.")]
    async fn memory_forget(&self, Parameters(input): Parameters<MemoryForgetInput>) -> String {
        let Some(mm) = &self.memory_manager else {
            return json_err("memory subsystem is not enabled");
        };

        let Some(id_str) = &input.memory_id else {
            return json_err("memory_id is required");
        };
        let id = match id_str.parse::<uuid::Uuid>() {
            Ok(id) => id,
            Err(e) => return json_err(&format!("invalid memory_id: {e}")),
        };

        let invalidate = input.invalidate.unwrap_or(true);
        let result = if invalidate {
            mm.invalidate_memory(id).await
        } else {
            mm.delete_memory(id).await
        };

        match result {
            Ok(done) => serde_json::to_string(&MemoryForgetOutput {
                forgotten: done,
                action: if invalidate { "invalidated" } else { "deleted" }.to_string(),
            })
            .unwrap_or_else(|e| json_err(&e.to_string())),
            Err(e) => json_err(&e.to_string()),
        }
    }

    #[tool(description = "Manage memory sessions: list active sessions, get session context, or end a session.")]
    async fn memory_sessions(&self, Parameters(input): Parameters<MemorySessionsInput>) -> String {
        let Some(mm) = &self.memory_manager else {
            return json_err("memory subsystem is not enabled");
        };

        match input.action.as_deref().unwrap_or("list") {
            "list" => {
                let sessions = mm.list_sessions();
                serde_json::to_string(&serde_json::json!({
                    "active_sessions": sessions,
                    "count": sessions.len(),
                }))
                .unwrap_or_else(|e| json_err(&e.to_string()))
            }
            "get" => {
                let Some(sid) = &input.session_id else {
                    return json_err("session_id is required for 'get' action");
                };
                match mm.get_session_context(sid).await {
                    Some(ctx) => serde_json::to_string(&serde_json::json!({
                        "session_id": ctx.session_id,
                        "message_count": ctx.message_count,
                        "created_at": ctx.created_at.to_rfc3339(),
                        "last_active": ctx.last_active.to_rfc3339(),
                        "has_summary": ctx.summary.is_some(),
                    }))
                    .unwrap_or_else(|e| json_err(&e.to_string())),
                    None => json_err(&format!("session not found: {sid}")),
                }
            }
            "end" => {
                let Some(sid) = &input.session_id else {
                    return json_err("session_id is required for 'end' action");
                };
                match mm.end_session(sid, true, input.user_id.as_deref()).await {
                    Ok(Some(entry)) => serde_json::to_string(&serde_json::json!({
                        "ended": true,
                        "persisted_memory_id": entry.id.to_string(),
                    }))
                    .unwrap_or_else(|e| json_err(&e.to_string())),
                    Ok(None) => serde_json::to_string(&serde_json::json!({ "ended": true }))
                        .unwrap_or_else(|e| json_err(&e.to_string())),
                    Err(e) => json_err(&e.to_string()),
                }
            }
            other => json_err(&format!("unknown action: {other}")),
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
            Annotated::new(
                RawResource::new(COMMUNITIES_RESOURCE_URI, "communities")
                    .with_title("Entity Communities")
                    .with_description(
                        "Detected entity communities with summaries and importance scores.",
                    )
                    .with_mime_type("application/json"),
                None,
            ),
            Annotated::new(
                RawResource::new(MEMORY_STATUS_RESOURCE_URI, "memory-status")
                    .with_title("Memory Status")
                    .with_description(
                        "Memory subsystem status: active sessions, memory counts, and behavioral protocol.",
                    )
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
            COMMUNITIES_RESOURCE_URI => self.resource_communities().await,
            MEMORY_STATUS_RESOURCE_URI => self.resource_memory_status().await,
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
            chunking_max_sentences: 20,
            chunking_filter_garbage: true,
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
            CommunityConfig::default(),
            None,
            crate::config::ChunkingStrategy::Fixed,
            0.25,
            20,
            true,
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

    /**
     * Helper that builds a `GetChunkInput` with sensible defaults so the
     * individual tests stay focused on the single field they are exercising.
     */
    fn make_get_chunk_input(
        file_path: Option<&str>,
        document_id: Option<&str>,
        chunk_index: i32,
    ) -> GetChunkInput {
        GetChunkInput {
            file_path: file_path.map(str::to_string),
            document_id: document_id.map(str::to_string),
            chunk_index,
            before: None,
            after: None,
        }
    }

    /**
     * Supplying neither `file_path` nor `document_id` must be rejected with
     * a structured JSON error rather than silently returning an empty
     * response.
     */
    #[tokio::test]
    async fn rag_get_chunk_requires_one_selector() {
        let (server, _tmp) = make_test_server().await;
        let result = server
            .rag_get_chunk(Parameters(make_get_chunk_input(None, None, 0)))
            .await;
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(json["error"].is_string(), "response must be an error: {result}");
        assert!(
            json["error"].as_str().unwrap().contains("exactly one"),
            "error should mention the selector contract: {result}"
        );
    }

    /**
     * Supplying both selectors simultaneously is also ambiguous -- the
     * handler must reject it to avoid the storage layer choosing one
     * silently.
     */
    #[tokio::test]
    async fn rag_get_chunk_rejects_both_selectors() {
        let (server, _tmp) = make_test_server().await;
        let id = uuid::Uuid::new_v4().to_string();
        let result = server
            .rag_get_chunk(Parameters(make_get_chunk_input(
                Some("docs/file.md"),
                Some(&id),
                0,
            )))
            .await;
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(json["error"].is_string());
    }

    /**
     * A malformed UUID in `document_id` must fail fast with a precise error
     * rather than being forwarded to the database.
     */
    #[tokio::test]
    async fn rag_get_chunk_rejects_invalid_document_id() {
        let (server, _tmp) = make_test_server().await;
        let result = server
            .rag_get_chunk(Parameters(make_get_chunk_input(
                None,
                Some("not-a-uuid"),
                0,
            )))
            .await;
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(
            json["error"]
                .as_str()
                .unwrap()
                .to_lowercase()
                .contains("document_id"),
            "error must mention document_id: {result}"
        );
    }

    /**
     * Negative `chunk_index` values are nonsensical (chunks are stored with
     * zero-based indices) and must be rejected at the handler boundary.
     */
    #[tokio::test]
    async fn rag_get_chunk_rejects_negative_index() {
        let (server, _tmp) = make_test_server().await;
        let result = server
            .rag_get_chunk(Parameters(make_get_chunk_input(
                Some("docs/file.md"),
                None,
                -1,
            )))
            .await;
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(json["error"]
            .as_str()
            .unwrap()
            .contains("chunk_index"));
    }

    /**
     * Querying a file_path with no stored chunks returns a valid empty
     * response (not an error) so agents can distinguish "unknown document"
     * from "server failure".
     */
    #[tokio::test]
    async fn rag_get_chunk_returns_empty_for_unknown_file_path() {
        let (server, _tmp) = make_test_server().await;
        let result = server
            .rag_get_chunk(Parameters(make_get_chunk_input(
                Some("docs/does-not-exist.md"),
                None,
                0,
            )))
            .await;
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(json.get("error").is_none(), "unexpected error: {result}");
        assert!(json["chunks"].is_array());
        assert!(json["chunks"].as_array().unwrap().is_empty());
    }

    // ─── rag_get_chunk end-to-end roundtrip tests ────────────────────────────

    /**
     * Ingests `text` as a single `text`-type document against the server's
     * storage backend and returns `(file_path, stored_chunk_count)`.
     *
     * The `Fixed` chunking strategy is used with a conservative max-sentence
     * limit so each paragraph becomes its own chunk; this makes the
     * neighbour-window assertions in the tests below deterministic.
     */
    async fn ingest_text(
        server: &WendRagServer,
        file_name: &str,
        text: &str,
    ) -> (String, usize) {
        use crate::ingest::pipeline;
        use crate::ingest::pipeline::{ContentIngestRequest, IngestOptions};

        let tags: Vec<String> = Vec::new();
        let file_path = format!("tests/{file_name}");
        let result = pipeline::ingest_content(
            &server.storage,
            &server.embedder,
            ContentIngestRequest {
                file_path: &file_path,
                file_name,
                file_type: "text",
                text,
            },
            &IngestOptions::new(
                None,
                &tags,
                None,
                None,
                crate::config::ChunkingStrategy::Fixed,
                0.25,
                20,
                false,
            ),
        )
        .await
        .expect("ingestion must succeed in tests");

        (file_path, result.chunk_count)
    }

    /**
     * Builds a document text whose paragraph-per-chunk layout is stable
     * and guarantees at least `min_chunks` distinct chunks.
     *
     * Each paragraph repeats a high-frequency filler enough times to push
     * past the fixed-window boundary, so the structural chunker cannot
     * merge adjacent paragraphs. The text-file limits (1 000 chars per
     * chunk) are documented in `retrieval.md`.
     */
    fn build_multi_chunk_document(min_chunks: usize) -> String {
        (0..min_chunks)
            .map(|i| format!("Section-{i} {}", "lorem ipsum dolor ".repeat(120)))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /**
     * Roundtrip by `file_path`: ingest a multi-chunk document, then fetch
     * an arbitrary interior chunk by index and confirm the response
     * includes matching identifiers and non-empty content.
     */
    #[tokio::test]
    async fn rag_get_chunk_returns_target_chunk_by_file_path() {
        let (server, _tmp) = make_test_server().await;
        let text = build_multi_chunk_document(5);
        let (file_path, chunk_count) = ingest_text(&server, "roundtrip-path.txt", &text).await;
        assert!(chunk_count >= 3, "fixture must produce at least 3 chunks");

        let target_index = 1;
        let response = server
            .rag_get_chunk(Parameters(make_get_chunk_input(
                Some(&file_path),
                None,
                target_index,
            )))
            .await;
        let json: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert!(json.get("error").is_none(), "unexpected error: {response}");
        let chunks = json["chunks"].as_array().expect("chunks must be an array");
        assert_eq!(chunks.len(), 1, "single-index window returns one chunk");

        let chunk = &chunks[0];
        assert_eq!(chunk["file_path"], file_path);
        assert_eq!(chunk["file_name"], "roundtrip-path.txt");
        assert_eq!(chunk["chunk_index"], target_index);
        assert!(
            chunk["document_id"].as_str().is_some(),
            "document_id must be populated: {chunk}"
        );
        assert!(
            chunk["content"]
                .as_str()
                .map(|c| !c.is_empty())
                .unwrap_or(false),
            "content must be non-empty: {chunk}"
        );
    }

    /**
     * Roundtrip by `document_id`: after ingestion, discover the document
     * UUID via `list_documents` and confirm the by-id lookup returns
     * exactly the same body as the by-path lookup.
     */
    #[tokio::test]
    async fn rag_get_chunk_returns_target_chunk_by_document_id() {
        let (server, _tmp) = make_test_server().await;
        let text = build_multi_chunk_document(4);
        let (file_path, _) = ingest_text(&server, "roundtrip-id.txt", &text).await;

        let docs = server
            .storage
            .list_documents(None, None)
            .await
            .expect("list_documents must succeed");
        let document_id = docs
            .iter()
            .find(|d| d.file_path == file_path)
            .expect("just-ingested document must appear in listing")
            .id
            .to_string();

        let by_path_json: serde_json::Value = serde_json::from_str(
            &server
                .rag_get_chunk(Parameters(make_get_chunk_input(Some(&file_path), None, 0)))
                .await,
        )
        .unwrap();
        let by_id_json: serde_json::Value = serde_json::from_str(
            &server
                .rag_get_chunk(Parameters(make_get_chunk_input(None, Some(&document_id), 0)))
                .await,
        )
        .unwrap();

        let by_path = &by_path_json["chunks"][0];
        let by_id = &by_id_json["chunks"][0];
        assert_eq!(by_path["content"], by_id["content"]);
        assert_eq!(by_path["document_id"], by_id["document_id"]);
        assert_eq!(by_path["chunk_index"], by_id["chunk_index"]);
    }

    /**
     * Requests with `before` and `after` must return the target chunk
     * plus the specified number of neighbours, in ascending chunk_index
     * order. Guards against regressions in the clipping / sorting logic.
     */
    #[tokio::test]
    async fn rag_get_chunk_returns_neighbours_in_order() {
        let (server, _tmp) = make_test_server().await;
        let text = build_multi_chunk_document(6);
        let (file_path, chunk_count) = ingest_text(&server, "window.txt", &text).await;
        assert!(chunk_count >= 5);

        let target_index = 2;
        let before = 1;
        let after = 2;
        let response = server
            .rag_get_chunk(Parameters(GetChunkInput {
                file_path: Some(file_path.clone()),
                document_id: None,
                chunk_index: target_index,
                before: Some(before),
                after: Some(after),
            }))
            .await;
        let json: serde_json::Value = serde_json::from_str(&response).unwrap();
        let chunks = json["chunks"].as_array().unwrap();

        assert_eq!(
            chunks.len(),
            (before + after + 1) as usize,
            "must return target + {before} before + {after} after: {response}"
        );

        let indices: Vec<i64> = chunks
            .iter()
            .map(|c| c["chunk_index"].as_i64().unwrap())
            .collect();
        let expected: Vec<i64> = ((target_index as i64 - before as i64)
            ..=(target_index as i64 + after as i64))
            .collect();
        assert_eq!(
            indices, expected,
            "chunks must be in ascending chunk_index order"
        );

        for chunk in chunks {
            assert_eq!(chunk["file_path"], file_path);
        }
    }

    /**
     * `before` / `after` requests larger than `MAX_CHUNK_CONTEXT` must be
     * silently clamped so a single call can never return more than
     * `2 * MAX_CHUNK_CONTEXT + 1` chunks. Guards against an accidental
     * raise of the server-side bound.
     */
    #[tokio::test]
    async fn rag_get_chunk_clamps_excessive_before_after() {
        let (server, _tmp) = make_test_server().await;
        // Ask for more than MAX_CHUNK_CONTEXT on both sides even though
        // the document doesn't have that many chunks; the test is about
        // the *cap*, not the clip at the document edge, so we verify
        // that the response size never exceeds the hard ceiling.
        let text = build_multi_chunk_document(50);
        let (file_path, chunk_count) = ingest_text(&server, "clamp.txt", &text).await;
        assert!(chunk_count >= 40);

        let target_index = 20;
        let response = server
            .rag_get_chunk(Parameters(GetChunkInput {
                file_path: Some(file_path),
                document_id: None,
                chunk_index: target_index,
                before: Some(MAX_CHUNK_CONTEXT + 50),
                after: Some(MAX_CHUNK_CONTEXT + 50),
            }))
            .await;
        let json: serde_json::Value = serde_json::from_str(&response).unwrap();
        let chunks = json["chunks"].as_array().unwrap();

        let max_allowed = (MAX_CHUNK_CONTEXT as usize) * 2 + 1;
        assert!(
            chunks.len() <= max_allowed,
            "response must honour the clamp (got {} chunks, max {})",
            chunks.len(),
            max_allowed
        );
    }

    /**
     * A `before` window that would run past chunk 0 must clip to chunk 0
     * instead of producing negative indices or a database error.
     */
    #[tokio::test]
    async fn rag_get_chunk_clips_window_at_document_start() {
        let (server, _tmp) = make_test_server().await;
        let text = build_multi_chunk_document(5);
        let (file_path, _) = ingest_text(&server, "start-clip.txt", &text).await;

        let response = server
            .rag_get_chunk(Parameters(GetChunkInput {
                file_path: Some(file_path),
                document_id: None,
                chunk_index: 0,
                before: Some(3),
                after: Some(1),
            }))
            .await;
        let json: serde_json::Value = serde_json::from_str(&response).unwrap();
        let chunks = json["chunks"].as_array().unwrap();

        // Document has chunks [0..N]. Asking for 3 before chunk 0 must
        // clip to just chunks [0, 1] -- the target plus the one after.
        assert_eq!(chunks.len(), 2, "start-of-document clip must yield 2 chunks");
        assert_eq!(chunks[0]["chunk_index"], 0);
        assert_eq!(chunks[1]["chunk_index"], 1);
    }

    /**
     * Scenario test for the Mermaid-diagram-split use case: a fenced code
     * block that would not fit in a single chunk gets recombined by
     * issuing a single `rag_get_chunk` call with a symmetric window.
     *
     * This is the intended flow described in `mcp-tools.md`: the agent
     * sees a partial code block in a `rag_get_context` hit, notes its
     * `file_path` + `chunk_index`, and asks for the surrounding chunks to
     * reconstruct the full block.
     */
    #[tokio::test]
    async fn rag_get_chunk_reconstructs_code_block_split_across_chunks() {
        let (server, _tmp) = make_test_server().await;

        // Build a document whose middle contains a large fenced block and
        // whose before / after paragraphs are wide enough to force each
        // into its own chunk. Marker sentinels make the reconstruction
        // assertion unambiguous.
        let filler_before = format!("PREFIX_MARKER {}", "before ".repeat(150));
        let diagram = format!(
            "```mermaid\n{}\n```",
            (0..120)
                .map(|i| format!("  node{i} --> node{}", i + 1))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let filler_after = format!("SUFFIX_MARKER {}", "after ".repeat(150));
        let text = format!("{filler_before}\n\n{diagram}\n\n{filler_after}");

        let (file_path, chunk_count) = ingest_text(&server, "mermaid.txt", &text).await;
        assert!(
            chunk_count >= 3,
            "document must produce at least 3 chunks for this scenario, got {chunk_count}"
        );

        // Pick the middle chunk as the one the agent saw truncated.
        let target_index = (chunk_count / 2) as i32;
        let response = server
            .rag_get_chunk(Parameters(GetChunkInput {
                file_path: Some(file_path.clone()),
                document_id: None,
                chunk_index: target_index,
                before: Some(MAX_CHUNK_CONTEXT),
                after: Some(MAX_CHUNK_CONTEXT),
            }))
            .await;
        let json: serde_json::Value = serde_json::from_str(&response).unwrap();
        let chunks = json["chunks"].as_array().unwrap();
        assert!(!chunks.is_empty(), "window must return at least one chunk");

        // Concatenate the returned chunk contents to confirm the agent
        // can rebuild the original diagram text from a single tool call.
        let reconstructed: String = chunks
            .iter()
            .map(|c| c["content"].as_str().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            reconstructed.contains("```mermaid"),
            "reconstruction must include the opening fence: first 300 chars = {:?}",
            &reconstructed.chars().take(300).collect::<String>()
        );
        assert!(
            reconstructed.contains("node0 --> node1"),
            "reconstruction must include the first edge of the diagram"
        );
        assert!(
            reconstructed.contains("```"),
            "reconstruction must include a closing fence"
        );
    }
}
