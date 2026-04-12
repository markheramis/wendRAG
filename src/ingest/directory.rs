/**
 * Directory-level ingestion: glob discovery, concurrent file processing,
 * orphan cleanup, and aggregate status reporting.
 */

use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::embed::EmbeddingProvider;
use crate::store::StorageBackend;

use super::reader::detect_file_type;
use super::pipeline::ingest_file;
use super::types::{
    DirectoryIngestRequest, IngestDocumentStatus, IngestError, IngestOptions, IngestPathResult,
    IngestResult, IngestStatus,
};

/// Maximum number of files ingested concurrently in multi-file flows.
const MAX_CONCURRENT_INGESTS: usize = 4;

/**
 * Ingests all supported files in a directory, optionally recursively and with
 * a caller-provided glob filter. Unsupported file types are ignored.
 *
 * Up to [`MAX_CONCURRENT_INGESTS`] files are processed in parallel. Results
 * are returned in the original filesystem discovery order.
 */
pub async fn ingest_directory(
    storage: &Arc<dyn StorageBackend>,
    embedder: &Arc<dyn EmbeddingProvider>,
    request: DirectoryIngestRequest<'_>,
    options: &IngestOptions,
) -> Result<IngestPathResult, IngestError> {
    let pattern = build_directory_pattern(
        request.directory_path,
        request.recursive,
        request.glob_pattern,
    );
    let entries =
        glob::glob(&pattern).map_err(|error| IngestError::InvalidGlobPattern(error.to_string()))?;

    let mut file_paths: Vec<String> = Vec::new();
    let mut glob_failures: usize = 0;

    for entry in entries {
        match entry {
            Ok(path) if path.is_file() => {
                let path_string = path.to_string_lossy().into_owned();
                if detect_file_type(&path_string).is_some() {
                    file_paths.push(path_string);
                }
            }
            Ok(_) => {}
            Err(_) => glob_failures += 1,
        }
    }

    tracing::info!(
        directory = %request.directory_path,
        file_count = file_paths.len(),
        "discovered files for ingestion"
    );

    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_INGESTS));
    let mut tasks: JoinSet<(usize, String, Result<IngestResult, IngestError>)> = JoinSet::new();

    let options = options.clone();
    let total_files = file_paths.len();

    for (index, path_string) in file_paths.iter().enumerate() {
        let storage = storage.clone();
        let embedder = embedder.clone();
        let path_string = path_string.clone();
        let options = options.clone();
        let permit = semaphore.clone().acquire_owned().await.unwrap();

        tasks.spawn(async move {
            tracing::info!(
                file = %path_string,
                progress = format!("[{}/{}]", index + 1, total_files),
                "ingesting"
            );
            let result = ingest_file(
                &storage,
                &embedder,
                options.entity_extractor.as_ref(),
                &path_string,
                options.project.as_deref(),
                &options.tags,
                options.chunking_strategy,
                options.semantic_threshold,
            )
            .await;
            drop(permit);
            (index, path_string, result)
        });
    }

    let mut indexed_results: Vec<(usize, String, Result<IngestResult, IngestError>)> =
        Vec::with_capacity(file_paths.len());

    while let Some(join_result) = tasks.join_next().await {
        match join_result {
            Ok(tuple) => indexed_results.push(tuple),
            Err(join_error) => {
                tracing::error!("ingestion task panicked: {join_error}");
                glob_failures += 1;
            }
        }
    }

    indexed_results.sort_by_key(|(index, _, _)| *index);

    let mut output = IngestPathResult {
        added: 0,
        updated: 0,
        unchanged: 0,
        deleted: 0,
        failed: glob_failures,
        documents: Vec::with_capacity(indexed_results.len()),
    };

    for (_index, path_string, result) in indexed_results {
        push_ingest_status(&mut output, &path_string, result);
    }

    if request.delete_removed {
        let deleted = delete_orphaned_documents(
            storage,
            request.directory_path,
            &file_paths,
            &mut output,
        )
        .await?;
        output.deleted = deleted;
    }

    Ok(output)
}

/**
 * Ingests a single file path and wraps the result in the shared aggregate shape
 * used by both CLI and directory ingestion responses.
 */
pub(super) async fn ingest_single_path(
    storage: &Arc<dyn StorageBackend>,
    embedder: &Arc<dyn EmbeddingProvider>,
    file_path: &str,
    options: &IngestOptions,
) -> Result<IngestPathResult, IngestError> {
    tracing::info!(file = %file_path, "ingesting");
    let result = ingest_file(
        storage,
        embedder,
        options.entity_extractor.as_ref(),
        file_path,
        options.project.as_deref(),
        &options.tags,
        options.chunking_strategy,
        options.semantic_threshold,
    )
    .await?;

    let mut output = IngestPathResult {
        added: 0,
        updated: 0,
        unchanged: 0,
        deleted: 0,
        failed: 0,
        documents: Vec::with_capacity(1),
    };
    push_ingest_status(&mut output, file_path, Ok(result));
    Ok(output)
}

/**
 * Builds the filesystem glob pattern used for recursive or non-recursive
 * directory ingestion while preserving the MCP tool's current semantics.
 */
fn build_directory_pattern(
    directory_path: &str,
    recursive: bool,
    glob_pattern: Option<&str>,
) -> String {
    match glob_pattern {
        Some(pattern) if recursive => format!("{directory_path}/**/{pattern}"),
        Some(pattern) => format!("{directory_path}/{pattern}"),
        None if recursive => format!("{directory_path}/**/*"),
        None => format!("{directory_path}/*"),
    }
}

/**
 * Updates aggregate ingestion counters and records the per-document status line
 * for a file processed during CLI or MCP-driven ingestion. Logs each result to
 * stderr so the CLI user can follow progress in real time.
 */
pub(super) fn push_ingest_status(
    output: &mut IngestPathResult,
    file_path: &str,
    result: Result<IngestResult, IngestError>,
) {
    match result {
        Ok(result) => {
            let status = result.status.to_string();
            tracing::info!(
                file = %file_path,
                status = %status,
                chunks = result.chunk_count,
                "done"
            );
            match result.status {
                IngestStatus::Created => output.added += 1,
                IngestStatus::Updated => output.updated += 1,
                IngestStatus::Unchanged => output.unchanged += 1,
            }
            output.documents.push(IngestDocumentStatus {
                file_path: file_path.to_string(),
                status,
            });
        }
        Err(error) => {
            tracing::error!(file = %file_path, error = %error, "failed");
            output.failed += 1;
            output.documents.push(IngestDocumentStatus {
                file_path: file_path.to_string(),
                status: format!("error: {error}"),
            });
        }
    }
}

/**
 * Removes documents from storage whose paths start with the directory prefix
 * but were not discovered in the current file set. Records each deletion as a
 * "deleted" status entry in the output.
 */
async fn delete_orphaned_documents(
    storage: &Arc<dyn StorageBackend>,
    directory_path: &str,
    discovered_files: &[String],
    output: &mut IngestPathResult,
) -> Result<usize, IngestError> {
    let existing_docs = storage.list_documents(None, None).await?;

    let dir_prefix = normalize_dir_prefix(directory_path);
    let mut deleted_count = 0;

    for doc in existing_docs {
        let normalized_path = doc.file_path.replace('\\', "/");
        if !normalized_path.starts_with(&dir_prefix) {
            continue;
        }

        let is_present = discovered_files
            .iter()
            .any(|f| f.replace('\\', "/") == normalized_path);

        if !is_present {
            match storage
                .delete_document(Some(&doc.file_path), None)
                .await
            {
                Ok(Some(_)) => {
                    deleted_count += 1;
                    output.documents.push(IngestDocumentStatus {
                        file_path: doc.file_path,
                        status: "deleted".to_string(),
                    });
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(path = %doc.file_path, %error, "failed to delete orphaned document");
                    output.failed += 1;
                    output.documents.push(IngestDocumentStatus {
                        file_path: doc.file_path,
                        status: format!("error: {error}"),
                    });
                }
            }
        }
    }

    Ok(deleted_count)
}

/**
 * Normalizes a directory path to a forward-slash prefix ending with `/` for
 * consistent prefix matching across platforms.
 */
fn normalize_dir_prefix(directory_path: &str) -> String {
    let normalized = directory_path.replace('\\', "/");
    if normalized.ends_with('/') {
        normalized
    } else {
        format!("{normalized}/")
    }
}
