use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Serialize)]
pub struct FullContextOutput {
    pub mode: String,
    pub results: Vec<FullContextResultItem>,
}

#[derive(Debug, Serialize)]
pub struct FullContextResultItem {
    pub document_content: String,
    pub file_path: String,
    pub file_name: String,
    pub score: f64,
    pub matched_chunk_indices: Vec<i32>,
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
