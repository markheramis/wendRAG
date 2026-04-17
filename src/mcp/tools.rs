use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Maximum byte length of a single piece of user-supplied content (e.g. a
/// document body submitted via `rag_ingest` or `memory_store`).
///
/// 1 MiB is well above any practical markdown document yet low enough that a
/// single malicious request cannot exhaust the server's memory or provoke
/// a multi-minute embedding call.
pub const MAX_CONTENT_BYTES: usize = 1_048_576;

/// Maximum byte length of a search query. Queries are short by definition
/// (typically <200 chars); 10 KiB leaves comfortable headroom while blocking
/// prompt-stuffing attempts that would otherwise force an expensive
/// embedding call.
pub const MAX_QUERY_BYTES: usize = 10_240;

/// Maximum number of documents in a single `rag_ingest_batch` call. Each
/// document still enforces the per-item content limit, so the worst-case
/// total payload is `MAX_BATCH_ITEMS * MAX_CONTENT_BYTES`.
pub const MAX_BATCH_ITEMS: usize = 100;

/// Validates that `content` does not exceed [`MAX_CONTENT_BYTES`].
///
/// # Parameters
/// - `content`: The text to inspect.
/// - `field_name`: Human-readable name of the originating field, used in the
///   error message so operators can quickly locate the offending input.
///
/// # Errors
/// Returns a descriptive `String` when the size check fails. Callers should
/// forward the string through `json_err` so the MCP client receives a
/// structured error.
pub fn validate_content_size(content: &str, field_name: &str) -> Result<(), String> {
    if content.len() > MAX_CONTENT_BYTES {
        return Err(format!(
            "{field_name} exceeds maximum size: {} bytes > {} bytes",
            content.len(),
            MAX_CONTENT_BYTES
        ));
    }
    Ok(())
}

/// Validates that a search query does not exceed [`MAX_QUERY_BYTES`].
pub fn validate_query_size(query: &str) -> Result<(), String> {
    if query.len() > MAX_QUERY_BYTES {
        return Err(format!(
            "query exceeds maximum size: {} bytes > {} bytes",
            query.len(),
            MAX_QUERY_BYTES
        ));
    }
    Ok(())
}

/// Validates that `count` does not exceed [`MAX_BATCH_ITEMS`].
pub fn validate_batch_size(count: usize) -> Result<(), String> {
    if count > MAX_BATCH_ITEMS {
        return Err(format!(
            "batch contains {count} documents; maximum is {MAX_BATCH_ITEMS}"
        ));
    }
    Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IngestInput {
    /// Path to a file accessible to the server, or an HTTP(S) URL.
    /// Provide either file_path or content, not both.
    pub file_path: Option<String>,
    /// Inline text content to ingest. Requires file_name to determine type.
    pub content: Option<String>,
    /// Required when using inline content. Used to detect file type and as the document identity.
    pub file_name: Option<String>,
    /// Optional tags for categorization.
    pub tags: Option<Vec<String>>,
    /// Optional project namespace.
    pub project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IngestDirectoryInput {
    /// Path to a directory accessible to the server.
    pub directory_path: String,
    /// Whether to recurse into subdirectories. Defaults to true.
    pub recursive: Option<bool>,
    /// Glob pattern to filter files, e.g. "*.md". Applied within the directory.
    pub glob: Option<String>,
    /// Optional tags applied to all ingested documents.
    pub tags: Option<Vec<String>>,
    /// Optional project namespace.
    pub project: Option<String>,
    /// When true, remove documents whose source files no longer exist in the directory. Defaults to false.
    pub delete_removed: Option<bool>,
}

/// A single document in a batch ingestion request.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct IngestBatchItem {
    /// File name (with extension) used as document identity and to detect file type.
    pub file_name: String,
    /// Inline text content of the file.
    pub content: String,
}

/// Ingest multiple documents by inline content in a single call.
/// Designed for remote deployments where the server cannot access the client filesystem.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct IngestBatchInput {
    /// Array of documents to ingest.
    pub documents: Vec<IngestBatchItem>,
    /// Optional tags applied to all ingested documents.
    pub tags: Option<Vec<String>>,
    /// Optional project namespace.
    pub project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchInput {
    /// The search query string.
    pub query: String,
    /// Number of results to return. Defaults to 10.
    pub top_k: Option<i32>,
    /// Search mode: "hybrid" (default), "dense", or "sparse".
    pub mode: Option<String>,
    /// Filter results to these file types, e.g. ["markdown", "pdf", "url"].
    pub file_types: Option<Vec<String>>,
    /// Filter results to documents with any of these tags.
    pub tags: Option<Vec<String>>,
    /// Filter results to this project namespace.
    pub project: Option<String>,
    /// Minimum score threshold. Results below this are excluded.
    pub threshold: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListSourcesInput {
    /// Filter to this project namespace.
    pub project: Option<String>,
    /// Filter to this file type, e.g. "markdown" or "url".
    pub file_type: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteSourceInput {
    /// File path of the document to delete.
    pub file_path: Option<String>,
    /// UUID of the document to delete.
    pub document_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct IngestOutput {
    pub document_id: String,
    pub file_path: String,
    pub chunk_count: usize,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct IngestDirectoryOutput {
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub deleted: usize,
    pub failed: usize,
    pub documents: Vec<IngestDocStatus>,
}

#[derive(Debug, Serialize)]
pub struct IngestDocStatus {
    pub file_path: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct SearchOutput {
    pub mode: String,
    pub results: Vec<SearchResultItem>,
}

#[derive(Debug, Serialize)]
pub struct SearchResultItem {
    pub chunk_content: String,
    pub section_title: Option<String>,
    pub file_path: String,
    pub file_name: String,
    pub score: f64,
    pub chunk_index: i32,
}

/// Maximum number of surrounding chunks the caller may request on each
/// side of the target chunk via `rag_get_chunk`. Combined with the target
/// chunk this caps a single response at 21 chunks, which keeps the worst-
/// case payload (21 × 1 MiB per chunk) bounded and aligns with the other
/// MCP input size limits in this module.
pub const MAX_CHUNK_CONTEXT: u32 = 10;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetChunkInput {
    /// File path of the source document. Provide either `file_path` or
    /// `document_id`, not both.
    pub file_path: Option<String>,
    /// UUID of the source document. Provide either `file_path` or
    /// `document_id`, not both.
    pub document_id: Option<String>,
    /// Zero-based chunk index. Use the `chunk_index` value returned by
    /// `rag_get_context` to fetch that exact chunk.
    pub chunk_index: i32,
    /// Number of contiguous chunks to include BEFORE the target. Defaults
    /// to `0`. Capped at [`MAX_CHUNK_CONTEXT`] to bound response size.
    pub before: Option<u32>,
    /// Number of contiguous chunks to include AFTER the target. Defaults
    /// to `0`. Capped at [`MAX_CHUNK_CONTEXT`] to bound response size.
    pub after: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct GetChunkOutput {
    pub chunks: Vec<ChunkItem>,
}

/// Full content and identifying metadata for a single stored chunk. The
/// shape intentionally mirrors the `rag_get_context` result items plus
/// the document identifiers so agents can correlate responses across
/// tools without additional bookkeeping.
#[derive(Debug, Serialize)]
pub struct ChunkItem {
    pub document_id: String,
    pub file_path: String,
    pub file_name: String,
    pub chunk_index: i32,
    pub section_title: Option<String>,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct ListSourcesOutput {
    pub documents: Vec<DocumentInfo>,
}

#[derive(Debug, Serialize)]
pub struct DocumentInfo {
    pub id: String,
    pub file_path: String,
    pub file_name: String,
    pub file_type: String,
    pub project: Option<String>,
    pub tags: Vec<String>,
    pub chunk_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct DeleteSourceOutput {
    pub deleted: bool,
    pub file_path: Option<String>,
    pub chunk_count_removed: i64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoryStoreInput {
    /// Content to store as a memory.
    pub content: String,
    /// Scope: "session", "user", or "global". Defaults to "user".
    pub scope: Option<String>,
    /// Entry type: "fact", "preference", "event", "summary", or "message". Defaults to "fact".
    pub entry_type: Option<String>,
    /// User identifier for user-scoped memories.
    pub user_id: Option<String>,
    /// Session identifier for session-scoped memories.
    pub session_id: Option<String>,
    /// Importance score (0.0 to 1.0). Defaults to 0.5.
    pub importance: Option<f32>,
}

#[derive(Debug, Serialize)]
pub struct MemoryStoreOutput {
    pub memory_id: String,
    pub scope: String,
    pub entry_type: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoryRetrieveInput {
    /// Search query for semantic memory retrieval.
    pub query: String,
    /// Filter by user ID.
    pub user_id: Option<String>,
    /// Filter by session ID.
    pub session_id: Option<String>,
    /// Filter by scope: "session", "user", or "global".
    pub scope: Option<String>,
    /// Maximum results to return. Defaults to 10.
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct MemoryRetrieveOutput {
    pub memories: Vec<MemoryItem>,
}

#[derive(Debug, Serialize)]
pub struct MemoryItem {
    pub id: String,
    pub content: String,
    pub scope: String,
    pub entry_type: String,
    pub importance: f32,
    pub created_at: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoryForgetInput {
    /// Specific memory ID to forget.
    pub memory_id: Option<String>,
    /// If true, soft-delete (mark as invalidated) instead of hard delete. Defaults to true.
    pub invalidate: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct MemoryForgetOutput {
    pub forgotten: bool,
    pub action: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemorySessionsInput {
    /// Action: "list", "get", or "end". Defaults to "list".
    pub action: Option<String>,
    /// Session ID for "get" or "end" actions.
    pub session_id: Option<String>,
    /// User ID to associate with the session on "end" (for persisting summary).
    pub user_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /**
     * SEC-03: inputs at exactly the cap must pass; inputs one byte over must
     * fail. Boundary behavior matters because off-by-one bugs here would
     * either reject legitimate payloads or let attackers slip through with a
     * single padding byte.
     */
    #[test]
    fn validate_content_size_accepts_max_rejects_over() {
        let at_limit = "a".repeat(MAX_CONTENT_BYTES);
        assert!(validate_content_size(&at_limit, "content").is_ok());

        let over_limit = "a".repeat(MAX_CONTENT_BYTES + 1);
        let err = validate_content_size(&over_limit, "content").unwrap_err();
        assert!(err.contains("content"), "field name must appear: {err}");
        assert!(
            err.contains(&format!("{}", MAX_CONTENT_BYTES)),
            "limit must appear: {err}"
        );
    }

    /**
     * SEC-03: empty content is always acceptable; validation is purely a
     * size cap, not a "content is required" check.
     */
    #[test]
    fn validate_content_size_accepts_empty() {
        assert!(validate_content_size("", "content").is_ok());
    }

    /**
     * SEC-03: the field name supplied by the caller (e.g.
     * `"documents[3].content"`) must surface in the error so operators can
     * locate the offending entry in a batch request.
     */
    #[test]
    fn validate_content_size_preserves_field_name() {
        let over_limit = "a".repeat(MAX_CONTENT_BYTES + 1);
        let err = validate_content_size(&over_limit, "documents[3].content").unwrap_err();
        assert!(err.contains("documents[3].content"));
    }

    /**
     * SEC-03: query size boundary behaviour mirrors content validation but
     * at the tighter 10 KiB cap.
     */
    #[test]
    fn validate_query_size_accepts_max_rejects_over() {
        let at_limit = "q".repeat(MAX_QUERY_BYTES);
        assert!(validate_query_size(&at_limit).is_ok());

        let over_limit = "q".repeat(MAX_QUERY_BYTES + 1);
        assert!(validate_query_size(&over_limit).is_err());
    }

    /**
     * SEC-03: batch cap is inclusive of 100, exclusive above. A 101-item
     * batch must be rejected before any item is processed so a single bad
     * request cannot enqueue hundreds of embedding calls.
     */
    #[test]
    fn validate_batch_size_accepts_max_rejects_over() {
        assert!(validate_batch_size(MAX_BATCH_ITEMS).is_ok());
        assert!(validate_batch_size(0).is_ok());

        let err = validate_batch_size(MAX_BATCH_ITEMS + 1).unwrap_err();
        assert!(err.contains(&format!("{}", MAX_BATCH_ITEMS + 1)));
        assert!(err.contains(&format!("{}", MAX_BATCH_ITEMS)));
    }

    /**
     * SEC-03 regression: the published size limits must stay stable so
     * documentation, client SDKs, and server behaviour remain aligned.
     * A casual change to these constants will break this test and force
     * the author to update the docs in the same change.
     */
    #[test]
    fn size_limits_match_documented_values() {
        assert_eq!(MAX_CONTENT_BYTES, 1_048_576);
        assert_eq!(MAX_QUERY_BYTES, 10_240);
        assert_eq!(MAX_BATCH_ITEMS, 100);
    }
}
