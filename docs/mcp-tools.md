# MCP Tools Reference

wendRAG exposes seven MCP tools over Streamable HTTP (default endpoint: `http://localhost:3000/mcp`) or stdio.

## Ingestion tools

### `rag_ingest`

Ingests a single local file, an HTTP(S) URL, or inline text content.

| Parameter | Type | Notes |
|---|---|---|
| `file_path` | string | Local path or HTTP(S) URL to ingest. Omit when using `content`. |
| `content` | string | Inline document text. Provide `file_name` alongside for a stable identity. |
| `file_name` | string | Filename used for document identity when ingesting inline content. Falls back to `unnamed.txt` if omitted. |
| `file_type` | string | Optional override for document type detection. |
| `tags` | string[] | Optional labels attached to the document. |
| `project` | string | Optional project namespace for scoped search and listing. |

Re-ingestion is skipped automatically when the content hash is unchanged. URL ingestion stores the original URL as `file_path` and derives the document type as `url`. The fetcher checks `robots.txt` before downloading and surfaces HTTP rate-limiting without retry loops.

### `rag_ingest_directory`

Ingests all supported files (`.md`, `.txt`, `.pdf`) from a local directory.

| Parameter | Type | Notes |
|---|---|---|
| `directory` | string | Root directory to scan. |
| `recursive` | bool | Whether to descend into subdirectories (default `false`). |
| `glob` | string | Optional glob pattern to filter filenames. |
| `tags` | string[] | Optional labels applied to every ingested document. |
| `project` | string | Optional project namespace. |

Returns counts for `ingested`, `skipped`, and `failed` documents.

### `rag_ingest_batch`

Ingests multiple inline documents in a single call.

Each item in the `documents` array must provide:

| Field | Type | Notes |
|---|---|---|
| `file_name` | string | Filename used for document identity. |
| `content` | string | Document text. |
| `tags` | string[] | Optional labels. |
| `project` | string | Optional project namespace. |

---

## Search tools

### `rag_get_context`

Searches the index and returns chunk-level matches.

| Parameter | Type | Default | Notes |
|---|---|---|---|
| `query` | string | required | Search query. |
| `mode` | string | `hybrid` | `hybrid`, `dense`, or `sparse`. |
| `top_k` | int | `5` | Maximum number of chunks to return. |
| `file_types` | string[] | — | Filter by document type, e.g. `["markdown", "url"]`. |
| `tags` | string[] | — | Filter by tags. |
| `project` | string | — | Restrict to a project namespace. |
| `threshold` | float | — | Minimum score threshold (0.0–1.0). |

Returns an array of chunk-level results with `content`, `file_path`, `score`, and metadata.

### `rag_get_full_context`

Uses the same search inputs as `rag_get_context`, but collapses chunk hits to unique documents and returns reconstructed full-document context built from all stored ordered chunks for each matched document.

Because the original raw document body is not stored, the response is rebuilt from chunks with conservative overlap trimming. Use this when you need the full surrounding context of a match rather than isolated excerpts.

---

## Source management tools

### `rag_list_sources`

Lists all indexed documents.

| Parameter | Type | Notes |
|---|---|---|
| `project` | string | Optional project namespace filter. |
| `file_type` | string | Optional document type filter (e.g. `markdown`, `url`, `pdf`). |

Returns an array of documents with `file_path`, `file_name`, `file_type`, `chunk_count`, `tags`, `project`, `created_at`, and `updated_at`.

### `rag_delete_source`

Deletes a document and all its stored chunks.

| Parameter | Type | Notes |
|---|---|---|
| `file_path` | string | Path or URL of the document to delete. |
| `document_id` | string | UUID of the document. |

Provide at least one selector. If neither matches an existing document, the tool returns `deleted: false`.
