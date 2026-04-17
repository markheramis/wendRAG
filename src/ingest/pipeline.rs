/*!
 * Core single-document ingestion pipeline: hash-based short-circuit, chunking,
 * embedding, optional entity extraction, and storage upsert.
 */

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use futures::stream::{self, StreamExt, TryStreamExt};
use sha2::{Digest, Sha256};

/// Maximum number of concurrent LLM entity-extraction calls issued per
/// document. Four is a conservative default that noticeably accelerates
/// ingestion of long documents without overwhelming upstream rate limits.
const ENTITY_EXTRACTION_CONCURRENCY: usize = 4;

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
 *
 * The wide argument list mirrors the fully-populated `IngestOptions` that
 * the CLI / MCP layer assembles from config. A builder would just move the
 * wiring one level down without reducing the total surface area.
 */
#[allow(clippy::too_many_arguments)]
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
 *
 * Wide argument list mirrors `IngestOptions`; see `ingest_path` for the
 * rationale.
 */
#[allow(clippy::too_many_arguments)]
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
    let mut document_graph = if let Some(entity_extractor) = options.entity_extractor.as_ref() {
        // PERF-01: issue up to ENTITY_EXTRACTION_CONCURRENCY LLM calls
        // concurrently. Preserves input order because `buffered` returns
        // outputs in the same order as their futures were spawned, which
        // downstream graph construction relies on.
        //
        // The futures are materialised into a `Vec` first so the iterator's
        // borrow lifetime is decoupled from the stream's future lifetime --
        // without this intermediate collection the HRTB inference on
        // `buffered` fails ("FnOnce not general enough").
        let extractor = entity_extractor.as_ref();
        let extraction_futures: Vec<_> = raw_chunks
            .iter()
            .map(|chunk| {
                extractor.extract(EntityExtractionInput {
                    file_name: request.file_name,
                    file_type: request.file_type,
                    chunk_index: chunk.chunk_index,
                    section_title: chunk.section_title.as_deref(),
                    content: chunk.content.as_str(),
                })
            })
            .collect();
        let chunk_extractions: Vec<_> = stream::iter(extraction_futures)
            .buffered(ENTITY_EXTRACTION_CONCURRENCY)
            .try_collect()
            .await?;

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
    if let Some(document_graph) = document_graph.as_mut() {
        // After this call entity embeddings have been moved out of the
        // graph to avoid a per-entity clone (PERF-03). All other fields
        // remain intact so downstream community analysis still works.
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

/**
 * Computes the hex-encoded SHA-256 of `data`.
 *
 * PERF-06: the naive `.map(|b| format!("{b:02x}")).collect()` version
 * allocates a `String` per byte (32 allocations for a 256-bit digest). This
 * implementation pre-allocates a single 64-byte `String` and writes each
 * byte in place, eliminating the per-byte heap churn on a hot ingestion
 * path.
 */
fn sha256_hex(data: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    let digest = hasher.finalize();

    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(hex, "{byte:02x}").expect("writing to String is infallible");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::sha256_hex;

    /**
     * PERF-06 correctness guard: the pre-allocated hex encoder must
     * produce exactly the canonical SHA-256 digest of the empty string.
     * Digest value sourced from FIPS 180-4 / NIST test vectors.
     */
    #[test]
    fn sha256_hex_matches_known_empty_digest() {
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /**
     * Canonical `"abc"` vector from FIPS 180-4 §D.1.
     */
    #[test]
    fn sha256_hex_matches_known_abc_digest() {
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /**
     * Shape contract: every SHA-256 output is exactly 64 lowercase hex
     * characters, regardless of input length. This guards against
     * regressions in the zero-padding (`{:02x}`) formatter.
     */
    #[test]
    fn sha256_hex_output_is_always_64_lowercase_hex() {
        for input in ["", "a", "the quick brown fox", &"x".repeat(10_000)] {
            let digest = sha256_hex(input);
            assert_eq!(digest.len(), 64, "digest for {input:?} must be 64 chars");
            assert!(
                digest.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "digest for {input:?} must be lowercase hex: {digest}"
            );
        }
    }

    /**
     * Different inputs must produce different digests (sanity check on
     * the hasher wiring; a regression that fed constant data to
     * `hasher.update` would collapse every output to the same value).
     */
    #[test]
    fn sha256_hex_is_sensitive_to_input_changes() {
        assert_ne!(sha256_hex("foo"), sha256_hex("bar"));
        assert_ne!(sha256_hex("foo"), sha256_hex("foo "));
    }
}
