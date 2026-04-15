/**
 * Core single-document ingestion pipeline: hash-based short-circuit, chunking,
 * embedding, optional entity extraction, and storage upsert.
 */

use std::path::Path;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::config::ChunkingStrategy;
use crate::embed::EmbeddingProvider;
use crate::entity::{
    CommunityDetectionConfig, CommunityManager, EntityExtractionInput, EntityExtractor,
    MIN_ENTITIES_FOR_COMMUNITIES, build_document_entity_graph,
};
use crate::store::{ChunkInsert, DocumentUpsert, StorageBackend};

use super::chunker::{RawChunk, chunk_document};
use super::directory::ingest_single_path;
use super::reader::{detect_file_type, read_source_with_options};

pub use super::directory::ingest_directory;
pub use super::types::{
    ContentIngestRequest, DirectoryIngestRequest, IngestDocumentStatus, IngestError,
    IngestOptions, IngestPathResult, IngestResult, IngestStatus,
};

/**
 * Ingests either a single file or all supported files beneath a directory path.
 * Returns aggregate counters plus per-document status for CLI and MCP callers.
 */
pub async fn ingest_path(
    storage: &Arc<dyn StorageBackend>,
    embedder: &Arc<dyn EmbeddingProvider>,
    entity_extractor: Option<&Arc<dyn EntityExtractor>>,
    path: &str,
    project: Option<&str>,
    tags: &[String],
    chunking_strategy: ChunkingStrategy,
    semantic_threshold: f64,
    max_sentences: usize,
    filter_garbage: bool,
) -> Result<IngestPathResult, IngestError> {
    let options = IngestOptions::new(
        project,
        tags,
        entity_extractor,
        None,
        chunking_strategy,
        semantic_threshold,
        max_sentences,
        filter_garbage,
    );
    if detect_file_type(path) == Some("url") {
        return ingest_single_path(storage, embedder, path, &options).await;
    }

    let target_path = Path::new(path);

    if target_path.is_file() {
        return ingest_single_path(storage, embedder, path, &options).await;
    }

    if target_path.is_dir() {
        return ingest_directory(
            storage,
            embedder,
            DirectoryIngestRequest {
                directory_path: path,
                recursive: true,
                glob_pattern: None,
                delete_removed: false,
            },
            &options,
        )
        .await;
    }

    Err(IngestError::InvalidPath(path.to_string()))
}

/**
 * Ingests a file from disk by detecting its type, loading its contents, and
 * forwarding the normalized document to the shared content-ingestion flow.
 */
pub async fn ingest_file(
    storage: &Arc<dyn StorageBackend>,
    embedder: &Arc<dyn EmbeddingProvider>,
    entity_extractor: Option<&Arc<dyn EntityExtractor>>,
    file_path: &str,
    project: Option<&str>,
    tags: &[String],
    chunking_strategy: ChunkingStrategy,
    semantic_threshold: f64,
    max_sentences: usize,
    filter_garbage: bool,
) -> Result<IngestResult, IngestError> {
    let base_dir = std::path::Path::new(file_path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty());
    let source = read_source_with_options(file_path, base_dir, true).await?;
    let options = IngestOptions::new(
        project,
        tags,
        entity_extractor,
        None,
        chunking_strategy,
        semantic_threshold,
        max_sentences,
        filter_garbage,
    );

    ingest_content(
        storage,
        embedder,
        ContentIngestRequest {
            file_path,
            file_name: &source.file_name,
            file_type: source.file_type,
            text: &source.text,
        },
        &options,
    )
    .await
}

/**
 * Ingests inline content that has already been read by the caller, chunking it,
 * generating embeddings, and replacing the stored chunks for the document.
 */
pub async fn ingest_content(
    storage: &Arc<dyn StorageBackend>,
    embedder: &Arc<dyn EmbeddingProvider>,
    request: ContentIngestRequest<'_>,
    options: &IngestOptions,
) -> Result<IngestResult, IngestError> {
    let content_hash = sha256_hex(request.text);

    if let Some(existing) = storage.get_document_by_path(request.file_path).await?
        && existing.content_hash == content_hash
    {
        return Ok(IngestResult {
            document_id: existing.id,
            file_path: request.file_path.to_string(),
            chunk_count: 0,
            status: IngestStatus::Unchanged,
        });
    }

    let raw_chunks: Vec<RawChunk> = chunk_document(
        request.text,
        request.file_type,
        options.chunking_strategy,
        Some(embedder),
        options.semantic_threshold,
        options.max_sentences,
        options.filter_garbage,
    )
    .await?;

    let texts: Vec<String> = raw_chunks
        .iter()
        .map(|c| match &c.section_title {
            Some(title) => format!("{}\n\n{}", title, c.content),
            None => c.content.clone(),
        })
        .collect();

    let embeddings = embedder.embed(&texts).await?;
    let document_graph = if let Some(entity_extractor) = options.entity_extractor.as_ref() {
        let mut chunk_extractions = Vec::with_capacity(raw_chunks.len());
        for chunk in &raw_chunks {
            chunk_extractions.push(
                entity_extractor
                    .extract(EntityExtractionInput {
                        file_name: request.file_name,
                        file_type: request.file_type,
                        chunk_index: chunk.chunk_index,
                        section_title: chunk.section_title.as_deref(),
                        content: chunk.content.as_str(),
                    })
                    .await?,
            );
        }

        Some(build_document_entity_graph(&chunk_extractions, embedder).await?)
    } else {
        None
    };

    let doc_id = storage
        .upsert_document(&DocumentUpsert {
            file_path: request.file_path.to_string(),
            file_name: request.file_name.to_string(),
            file_type: request.file_type.to_string(),
            content_hash: content_hash.clone(),
            project: options.project.clone(),
            tags: options.tags.clone(),
        })
        .await?;

    let chunk_inserts: Vec<ChunkInsert> = raw_chunks
        .into_iter()
        .zip(embeddings)
        .map(|(rc, emb)| ChunkInsert {
            content: rc.content,
            chunk_index: rc.chunk_index,
            section_title: rc.section_title,
            embedding: emb,
        })
        .collect();

    let chunk_count = chunk_inserts.len();
    storage
        .replace_document_chunks(doc_id, &chunk_inserts)
        .await?;
    if let Some(document_graph) = document_graph.as_ref() {
        storage
            .replace_document_entity_graph(doc_id, document_graph)
            .await?;

        if document_graph.entities.len() >= MIN_ENTITIES_FOR_COMMUNITIES {
            let community_config = options
                .community_config
                .clone()
                .unwrap_or_default();
            let manager = CommunityManager::new(
                CommunityDetectionConfig::default(),
                community_config,
                embedder.clone(),
            );
            match manager.analyze_graph(document_graph).await {
                Ok(communities) => {
                    let project = options.project.as_deref();
                    if let Err(e) = storage.delete_project_communities(project).await {
                        tracing::warn!(error = %e, "failed to clear old communities");
                    }
                    if let Err(e) = storage.save_communities(project, &communities).await {
                        tracing::warn!(error = %e, "failed to persist communities");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "community detection failed, skipping");
                }
            }
        }
    }

    let was_update = storage
        .get_document_by_path(request.file_path)
        .await?
        .map(|d| d.created_at != d.updated_at)
        .unwrap_or(false);

    Ok(IngestResult {
        document_id: doc_id,
        file_path: request.file_path.to_string(),
        chunk_count,
        status: if was_update {
            IngestStatus::Updated
        } else {
            IngestStatus::Created
        },
    })
}

fn sha256_hex(data: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
