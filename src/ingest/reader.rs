use std::path::Path;

use url::Url;

use super::url::read_url_document;

pub const URL_FILE_TYPE: &str = "url";

#[derive(Debug, Clone)]
pub struct ReadDocument {
    pub file_name: String,
    pub file_type: &'static str,
    pub text: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("PDF extraction failed: {0}")]
    Pdf(String),
    #[error("invalid URL {path}: {reason}")]
    InvalidUrl { path: String, reason: String },
    #[error("robots.txt disallows URL ingestion: {0}")]
    RobotsDisallowed(String),
    #[error("URL request was rate limited for {url} (Retry-After: {retry_after:?})")]
    RateLimited {
        url: String,
        retry_after: Option<String>,
    },
    #[error("HTML conversion failed: {0}")]
    HtmlConversion(String),
    #[error("unsupported file type: {0}")]
    UnsupportedType(String),
}

/**
 * Detects the supported ingest type for local files and HTTP(S) URLs.
 *
 * Parameters:
 * - `path`: Local filesystem path or absolute HTTP(S) URL.
 *
 * Returns:
 * - Canonical file type string understood by the ingest pipeline.
 */
pub fn detect_file_type(path: &str) -> Option<&'static str> {
    if is_web_url(path) {
        return Some(URL_FILE_TYPE);
    }

    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());

    match ext.as_deref() {
        Some("md" | "markdown") => Some("markdown"),
        Some("txt" | "text") => Some("text"),
        Some("pdf") => Some("pdf"),
        _ => None,
    }
}

/**
 * Reads a supported ingest source and normalizes it into a shared document
 * shape for the downstream chunking and embedding pipeline.
 *
 * Parameters:
 * - `path`: Local filesystem path or absolute HTTP(S) URL.
 *
 * Returns:
 * - `ReadDocument` containing the normalized file name, file type, and text.
 */
pub async fn read_source(path: &str) -> Result<ReadDocument, ReadError> {
    let file_type =
        detect_file_type(path).ok_or_else(|| ReadError::UnsupportedType(path.to_string()))?;

    if file_type == URL_FILE_TYPE {
        return read_url_document(path).await;
    }

    let file_name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_string();
    match file_type {
        "markdown" | "text" => Ok(ReadDocument {
            file_name,
            file_type,
            text: std::fs::read_to_string(path)?,
        }),
        "pdf" => Ok(ReadDocument {
            file_name,
            file_type,
            text: read_pdf(path)?,
        }),
        _ => Err(ReadError::UnsupportedType(file_type.to_string())),
    }
}

/**
 * Extracts plain text from a PDF byte stream stored on disk.
 *
 * Parameters:
 * - `path`: Local filesystem path to the PDF.
 *
 * Returns:
 * - Extracted plain text for downstream chunking.
 */
fn read_pdf(path: &str) -> Result<String, ReadError> {
    let bytes = std::fs::read(path)?;
    pdf_extract::extract_text_from_mem(&bytes).map_err(|e| ReadError::Pdf(e.to_string()))
}

/**
 * Detects whether the input is an absolute HTTP or HTTPS URL.
 *
 * Parameters:
 * - `path`: Candidate input path.
 *
 * Returns:
 * - `true` when the input is a supported web URL.
 */
fn is_web_url(path: &str) -> bool {
    Url::parse(path)
        .ok()
        .is_some_and(|url| matches!(url.scheme(), "http" | "https"))
}
