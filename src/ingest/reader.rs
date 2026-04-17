use std::io::BufReader;
use std::path::{Path, PathBuf};

use url::Url;

use super::url::read_url_document_with_options;

pub const URL_FILE_TYPE: &str = "url";

/**
 * Validates that a file path is safe to read (prevents directory traversal).
 *
 * Ensures the resolved absolute path:
 * - Does not contain null bytes
 * - Is not a Windows UNC path pointing at a remote share (SEC-05)
 * - Exists and can be canonicalized (eliminates the TOCTOU symlink race
 *   in SEC-04)
 * - Stays inside the base directory after resolving symlinks and `..`
 *
 * Parameters:
 * - `path`: The user-provided path to validate.
 * - `base_dir`: Optional base directory that the path must stay within.
 *   If `None`, the current working directory is used.
 *
 * Returns:
 * - `Ok(PathBuf)` containing the canonicalized, verified path.
 * - `Err(ReadError)` if the path is invalid, missing, or escapes the base.
 *
 * # Security notes
 *
 * The previous implementation fell back to returning the non-canonical
 * absolute path when `canonicalize()` failed (e.g. because the file did
 * not yet exist). That fallback created a TOCTOU window in which a symlink
 * could be planted between the parent-directory check and the actual file
 * open. This function now requires the path to exist and canonicalize
 * cleanly, so the returned path is always the final target that will be
 * read by the caller.
 */
pub fn validate_safe_path(path: &str, base_dir: Option<&Path>) -> Result<PathBuf, ReadError> {
    if path.contains('\0') {
        return Err(ReadError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Path contains null bytes",
        )));
    }

    // SEC-05: reject Windows UNC paths early. `Path::is_absolute()` returns
    // `true` for these and `canonicalize()` may happily resolve them to a
    // remote SMB share, bypassing the base-directory containment check.
    #[cfg(windows)]
    {
        if path.starts_with("\\\\") || path.starts_with("//") {
            return Err(ReadError::InvalidPath(
                "UNC paths (\\\\server\\share) are not permitted".to_string(),
            ));
        }
    }

    let path_ref = Path::new(path);

    let base = match base_dir {
        Some(b) => b.to_path_buf(),
        None => std::env::current_dir().map_err(ReadError::Io)?,
    };

    let absolute_path = if path_ref.is_absolute() {
        path_ref.to_path_buf()
    } else {
        base.join(path_ref)
    };

    // SEC-04: require the path to exist and canonicalize. A failed
    // canonicalize means the file doesn't exist, and this function is only
    // used for *reading* existing files. Returning a non-canonical path
    // would create a TOCTOU window in which a symlink could be planted.
    let canonical_base = base.canonicalize().map_err(ReadError::Io)?;
    let canonical_path = absolute_path.canonicalize().map_err(|e| {
        ReadError::Io(std::io::Error::new(
            e.kind(),
            format!("could not resolve path '{}': {e}", absolute_path.display()),
        ))
    })?;

    if !canonical_path.starts_with(&canonical_base) {
        return Err(ReadError::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "Path escapes base directory",
        )));
    }

    Ok(canonical_path)
}

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
    #[error("DOCX extraction failed: {0}")]
    Docx(String),
    #[error("CSV parsing failed: {0}")]
    Csv(String),
    #[error("JSON parsing failed: {0}")]
    Json(String),
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
    #[error("path validation failed: {0}")]
    InvalidPath(String),
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
        Some("docx") => Some("docx"),
        Some("csv") => Some("csv"),
        Some("json") => Some("json"),
        _ => None,
    }
}

/**
 * Reads a supported ingest source and normalizes it into a shared document
 * shape for the downstream chunking and embedding pipeline.
 *
 * # Security
 *
 * For local files, this function validates the path to prevent directory
 * traversal attacks. The path must stay within the current working
 * directory or a specified `base_dir`.
 *
 * # Parameters
 *
 * - `path`: Local filesystem path or absolute HTTP(S) URL.
 * - `base_dir`: Optional base directory for path validation (defaults to
 *   the current working directory).
 * - `enforce_ssrf`: When true, URL ingestion blocks private / loopback /
 *   link-local addresses and installs a DNS-level re-validation resolver.
 *   Integration tests pass `false` to reach a local Axum test server.
 */
pub async fn read_source_with_options(
    path: &str,
    base_dir: Option<&Path>,
    enforce_ssrf: bool,
) -> Result<ReadDocument, ReadError> {
    let file_type =
        detect_file_type(path).ok_or_else(|| ReadError::UnsupportedType(path.to_string()))?;

    if file_type == URL_FILE_TYPE {
        return read_url_document_with_options(path, enforce_ssrf).await;
    }

    // SECURITY: Validate path to prevent directory traversal
    let validated_path = validate_safe_path(path, base_dir)?;

    let file_name = validated_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_string();

    let path_str = validated_path.to_str().ok_or_else(|| {
        ReadError::InvalidPath("Path contains invalid Unicode characters".to_string())
    })?;

    match file_type {
        "markdown" | "text" => Ok(ReadDocument {
            file_name,
            file_type,
            text: std::fs::read_to_string(&validated_path)?,
        }),
        "pdf" => Ok(ReadDocument {
            file_name,
            file_type,
            text: read_pdf(path_str)?,
        }),
        "docx" => Ok(ReadDocument {
            file_name,
            file_type,
            text: read_docx(path_str)?,
        }),
        "csv" => Ok(ReadDocument {
            file_name,
            file_type,
            text: read_csv(path_str)?,
        }),
        "json" => Ok(ReadDocument {
            file_name,
            file_type,
            text: read_json(path_str)?,
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

/**
 * Extracts plain text from a DOCX file.
 *
 * Parameters:
 * - `path`: Local filesystem path to the DOCX file.
 *
 * Returns:
 * - Extracted plain text with paragraph breaks preserved.
 */
fn read_docx(path: &str) -> Result<String, ReadError> {
    use docx_rs::{DocumentChild, ParagraphChild, RunChild};

    let bytes = std::fs::read(path)?;
    let doc = docx_rs::read_docx(&bytes).map_err(|e| ReadError::Docx(e.to_string()))?;

    let mut text = String::new();

    // docx-rs Document has a children field containing document elements
    for child in &doc.document.children {
        if let DocumentChild::Paragraph(paragraph) = child {
            let mut para_text = String::new();

            // Iterate through paragraph children to find text runs
            for p_child in &paragraph.children {
                if let ParagraphChild::Run(run) = p_child {
                    for run_child in &run.children {
                        if let RunChild::Text(text_elem) = run_child {
                            para_text.push_str(&text_elem.text);
                        }
                    }
                }
            }

            if !para_text.trim().is_empty() {
                text.push_str(&para_text);
                text.push('\n');
            }
        }
    }

    Ok(text.trim().to_string())
}

/**
 * Converts CSV rows to text chunks with structural context.
 *
 * Each row is formatted as: "Column1: Value1, Column2: Value2, ..."
 * Header row is preserved as column names.
 *
 * Parameters:
 * - `path`: Local filesystem path to the CSV file.
 *
 * Returns:
 * - Text representation of CSV data with headers and rows.
 */
fn read_csv(path: &str) -> Result<String, ReadError> {
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut csv_reader = csv::Reader::from_reader(reader);

    let headers = csv_reader
        .headers()
        .map_err(|e| ReadError::Csv(e.to_string()))?
        .clone();

    let mut result = String::new();

    // Add header context
    result.push_str("CSV Columns: ");
    result.push_str(&headers.iter().collect::<Vec<_>>().join(", "));
    result.push_str("\n\nRows:\n");

    for (row_num, record) in csv_reader.records().enumerate() {
        let record = record.map_err(|e| ReadError::Csv(e.to_string()))?;
        result.push_str(&format!("Row {}: ", row_num + 1));

        let mut fields = Vec::new();
        for (i, field) in record.iter().enumerate() {
            if let Some(header) = headers.get(i) {
                fields.push(format!("{}: {}", header, field));
            }
        }
        result.push_str(&fields.join(", "));
        result.push('\n');
    }

    Ok(result)
}

/**
 * Converts JSON objects to text chunks with structural context.
 *
 * For JSON arrays, each object is formatted as key-value pairs.
 * For single objects, all key-value pairs are extracted.
 * Nested objects are flattened with dot notation.
 *
 * Parameters:
 * - `path`: Local filesystem path to the JSON file.
 *
 * Returns:
 * - Text representation of JSON data.
 */
fn read_json(path: &str) -> Result<String, ReadError> {
    let content = std::fs::read_to_string(path)?;
    let value: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| ReadError::Json(e.to_string()))?;

    let mut result = String::new();

    match value {
        serde_json::Value::Array(arr) => {
            for (i, item) in arr.iter().enumerate() {
                result.push_str(&format!("Entry {}:\n", i + 1));
                result.push_str(&json_value_to_text(item, 0));
                result.push_str("\n\n");
            }
        }
        _ => {
            result.push_str(&json_value_to_text(&value, 0));
        }
    }

    Ok(result.trim().to_string())
}

/**
 * Recursively converts a JSON value to text representation.
 *
 * Parameters:
 * - `value`: The JSON value to convert.
 * - `indent`: Current indentation level for nested objects.
 *
 * Returns:
 * - Text representation of the JSON value.
 */
fn json_value_to_text(value: &serde_json::Value, indent: usize) -> String {
    let indent_str = "  ".repeat(indent);

    match value {
        serde_json::Value::Object(map) => {
            let mut lines = Vec::new();
            for (key, val) in map.iter() {
                match val {
                    serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                        lines.push(format!("{}{}:", indent_str, key));
                        lines.push(json_value_to_text(val, indent + 1));
                    }
                    _ => {
                        lines.push(format!("{}{}: {}", indent_str, key, json_scalar_to_string(val)));
                    }
                }
            }
            lines.join("\n")
        }
        serde_json::Value::Array(arr) => {
            let mut lines = Vec::new();
            for (i, item) in arr.iter().enumerate() {
                lines.push(format!("{}  [{}]:", indent_str, i));
                lines.push(json_value_to_text(item, indent + 2));
            }
            lines.join("\n")
        }
        _ => format!("{}{}", indent_str, json_scalar_to_string(value)),
    }
}

/**
 * Converts a scalar JSON value to string representation.
 *
 * Parameters:
 * - `value`: The scalar JSON value (string, number, bool, or null).
 *
 * Returns:
 * - String representation of the value.
 */
fn json_scalar_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_string(),
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /**
     * SEC-04 regression: a path that does not exist must fail validation.
     *
     * Previously `validate_safe_path` fell back to returning the
     * non-canonical absolute path when `canonicalize()` failed. That
     * fallback created a TOCTOU window where a symlink could be planted
     * between the parent-directory check and the subsequent file open.
     */
    #[test]
    fn validate_safe_path_rejects_nonexistent_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let missing = tmp.path().join("does-not-exist.txt");

        let result = validate_safe_path(
            missing.to_str().unwrap(),
            Some(tmp.path()),
        );
        assert!(
            result.is_err(),
            "expected a non-existent path to fail validation"
        );
    }

    /**
     * Files that do exist inside the base directory must resolve to an
     * absolute, canonicalised path that starts with the canonical base.
     */
    #[test]
    fn validate_safe_path_accepts_existing_file_inside_base() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("ok.txt");
        fs::write(&file, b"hello").unwrap();

        let resolved =
            validate_safe_path(file.to_str().unwrap(), Some(tmp.path())).unwrap();

        assert!(resolved.exists());
        let canonical_base = tmp.path().canonicalize().unwrap();
        assert!(
            resolved.starts_with(&canonical_base),
            "{resolved:?} must live inside {canonical_base:?}"
        );
    }

    /**
     * SEC-04: a path that, after canonicalisation, lies outside the base
     * directory must be rejected. Uses two sibling temp dirs so the
     * escape path is the canonical absolute form of the outsider.
     */
    #[test]
    fn validate_safe_path_rejects_escape_via_absolute_path() {
        let base = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        let outsider = outside.path().join("outside.txt");
        fs::write(&outsider, b"secret").unwrap();

        let result = validate_safe_path(
            outsider.to_str().unwrap(),
            Some(base.path()),
        );
        assert!(
            result.is_err(),
            "absolute path outside base must be rejected"
        );
    }

    /**
     * Inputs containing NUL bytes must be rejected up front. NUL
     * truncation is a classic trick to disguise an attack path from
     * filters that operate on `&str`.
     */
    #[test]
    fn validate_safe_path_rejects_null_bytes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = validate_safe_path("inno\0cent.txt", Some(tmp.path()));
        assert!(result.is_err(), "null-byte paths must be rejected");
    }

    /**
     * SEC-05: Windows UNC paths (`\\server\share\...`) must be rejected
     * before `canonicalize()` has a chance to resolve them to a remote
     * filesystem. On non-Windows targets this guard is inactive, which
     * is exactly the behaviour we want to assert here by gating on
     * `cfg(windows)`.
     */
    #[cfg(windows)]
    #[test]
    fn validate_safe_path_rejects_unc_paths_on_windows() {
        let tmp = tempfile::TempDir::new().unwrap();

        let backslash = validate_safe_path(
            r"\\server\share\secrets.txt",
            Some(tmp.path()),
        );
        assert!(
            backslash.is_err(),
            "backslash UNC paths must be rejected on Windows"
        );

        let forward_slash = validate_safe_path(
            "//server/share/secrets.txt",
            Some(tmp.path()),
        );
        assert!(
            forward_slash.is_err(),
            "forward-slash UNC paths must be rejected on Windows"
        );
    }
}
