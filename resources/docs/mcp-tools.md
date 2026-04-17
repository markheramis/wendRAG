# MCP Tools Reference

wendRAG exposes its tools over **Streamable HTTP** (default endpoint:
`http://localhost:3000/mcp`) or **stdio**.

Seven tools are always available for retrieval and ingestion
(`rag_*`). Four additional tools (`memory_*`) are registered when
`WEND_RAG_MEMORY_ENABLED=true`.

| Tool | Purpose |
|---|---|
| `rag_ingest` | Ingest a single file, URL, or inline text |
| `rag_ingest_directory` | Ingest every supported file under a directory |
| `rag_ingest_batch` | Ingest many inline documents in one call |
| `rag_get_context` | Search and return scored chunk-level matches |
| `rag_get_chunk` | Fetch one or more specific chunks by index |
| `rag_list_sources` | List indexed documents |
| `rag_delete_source` | Delete a document and its chunks |

## Authentication

- **stdio**: no auth; trust is inherited from the parent process.
- **HTTP**: Bearer token is **optional**. When `WEND_RAG_API_KEY` or any
  key from `wend-rag key:generate` is configured, every `/mcp` request
  must include `Authorization: Bearer <token>`. See
  [authentication-setup.md](authentication-setup.md) and
  [mcp-client-setup.md](mcp-client-setup.md) for per-client setup.

## Input size limits

Every tool handler enforces the following caps before doing any
database or embedding work. Requests exceeding a cap are rejected
with a descriptive JSON error; the field name (e.g.
`documents[3].content`) is included to help locate the offender.

| Field | Limit |
|---|---|
| `content` (in `rag_ingest`, `memory_store`, each `rag_ingest_batch.documents[*]`) | 1 MiB |
| `query` (in `rag_get_context`, `memory_retrieve`) | 10 KiB |
| `documents` array (in `rag_ingest_batch`) | 100 items |

See [configuration.md](configuration.md#input-size-limits-server-side)
for the rationale.

---

## Ingestion tools

### `rag_ingest`

Ingests a single local file, an HTTP(S) URL, or inline text content.

| Parameter | Type | Notes |
|---|---|---|
| `file_path` | string | Local path or HTTP(S) URL to ingest. Omit when using `content`. |
| `content` | string | Inline document text (≤ 1 MiB). Provide `file_name` alongside for a stable identity. |
| `file_name` | string | Filename used for document identity when ingesting inline content. Falls back to `unnamed.txt` if omitted. |
| `file_type` | string | Optional override for document type detection. |
| `tags` | string[] | Optional labels attached to the document. |
| `project` | string | Optional project namespace for scoped search and listing. |

Re-ingestion is skipped automatically when the content hash is unchanged. URL ingestion stores the original URL as `file_path` and derives the document type as `url`. The fetcher checks `robots.txt` before downloading, enforces an SSRF blocklist against private / loopback / link-local ranges (including IPv4-mapped IPv6 and decimal-encoded IPv4 forms), and re-validates every resolved IP at connection time to prevent DNS rebinding.

### `rag_ingest_directory`

Ingests all supported files (`.md`, `.txt`, `.pdf`, `.docx`, `.csv`, `.json`) from a local directory.

| Parameter | Type | Notes |
|---|---|---|
| `directory_path` | string | Root directory to scan. |
| `recursive` | bool | Whether to descend into subdirectories (default `true`). |
| `glob` | string | Optional glob pattern to filter filenames. |
| `tags` | string[] | Optional labels applied to every ingested document. |
| `project` | string | Optional project namespace. |
| `delete_removed` | bool | When `true`, remove documents whose source files no longer exist under the directory. Default `false`. |

Returns counts for `added`, `updated`, `unchanged`, `deleted`, and `failed` documents along with a per-document status list.

### `rag_ingest_batch`

Ingests multiple inline documents in a single call.

| Parameter | Type | Notes |
|---|---|---|
| `documents` | array | Up to 100 entries (see *Input size limits*). |
| `tags` | string[] | Optional labels applied to every document. |
| `project` | string | Optional project namespace. |

Each entry in `documents` must provide:

| Field | Type | Notes |
|---|---|---|
| `file_name` | string | Filename used for document identity. |
| `content` | string | Document text (≤ 1 MiB per entry). |

---

## Search tools

### `rag_get_context`

Searches the index and returns chunk-level matches.

| Parameter | Type | Default | Notes |
|---|---|---|---|
| `query` | string | required | Search query (≤ 10 KiB). |
| `mode` | string | `hybrid` | `hybrid`, `dense`, or `sparse`. |
| `top_k` | int | `10` | Maximum number of chunks to return. |
| `file_types` | string[] | — | Filter by document type, e.g. `["markdown", "url"]`. |
| `tags` | string[] | — | Filter by tags. |
| `project` | string | — | Restrict to a project namespace. |
| `threshold` | float | — | Minimum score threshold (0.0–1.0). |

Returns an array of chunk-level results with `chunk_content`, `file_path`, `score`, and metadata.

### `rag_get_chunk`

Fetches one or more specific stored chunks by chunk index. Useful when
`rag_get_context` surfaces a partial match and the agent needs the
exact neighbouring content to reconstruct it -- for example, a Mermaid
diagram code block that the chunker split across two or three chunks.

Exactly one of `file_path` or `document_id` must be supplied. The
values of `file_path` / `document_id` / `chunk_index` returned by
`rag_get_context` can be passed straight back in; the response carries
the same identifiers.

| Parameter | Type | Default | Notes |
|---|---|---|---|
| `file_path` | string | — | Path (or URL) of the source document. Mutually exclusive with `document_id`. |
| `document_id` | string (UUID) | — | Document UUID. Mutually exclusive with `file_path`. |
| `chunk_index` | int | required | Zero-based target chunk. |
| `before` | int | `0` | Contiguous chunks to include *before* the target (clamped to 10). |
| `after` | int | `0` | Contiguous chunks to include *after* the target (clamped to 10). |

With `before` and `after` combined, a single call returns at most 21
chunks, keeping the worst-case payload bounded even when every chunk
is at the 1 MiB content cap.

Response shape:

```json
{
  "chunks": [
    {
      "document_id": "...",
      "file_path": "...",
      "file_name": "...",
      "chunk_index": 4,
      "section_title": "Architecture",
      "content": "..."
    }
  ]
}
```

Chunks are always returned in ascending `chunk_index` order. Windows
that extend past the start or end of the document are clipped silently
rather than reported as an error.

**Typical workflow:**

1. Call `rag_get_context` with the user's query.
2. In the response, spot a chunk whose content is truncated mid-code-block,
   mid-table, or mid-diagram.
3. Re-issue `rag_get_chunk` with that chunk's `file_path` / `chunk_index`
   and set `before: 1, after: 1` (or wider) to pull the surrounding
   chunks and reconstruct the original block.

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

---

## Memory tools

These tools are only registered when the memory subsystem is enabled
(`WEND_RAG_MEMORY_ENABLED=true`). See [memory-sessions.md](memory-sessions.md)
for the architecture, scoping model, and decay/invalidation rules.

### `memory_store`

Store a memory entry (fact, preference, event, summary, or message) for later retrieval. Content is embedded and persisted alongside scope, type, and importance metadata.

### `memory_retrieve`

Search stored memories using semantic similarity, scoring by relevance and recency. Supports filters on user, session, and scope.

### `memory_forget`

Delete or soft-invalidate a memory entry. Soft-deleted entries are excluded from queries and hard-deleted by the maintenance task after 7 days.

### `memory_sessions`

List active sessions, inspect a single session, or end a session (optionally persisting its summary to long-term memory).

---

## Related resources

wendRAG also publishes read-only MCP resources:

| URI | Purpose |
|---|---|
| `rag://config` | Display-safe snapshot of the running configuration (secrets omitted). |
| `rag://status` | Document and chunk counts. |
| `rag://documents` | List of indexed documents. |
| `rag://documents/{id}` | Full metadata for a single document. |
| `rag://communities` | Detected entity communities (requires entity extraction). |
| `rag://memory/status` | Memory subsystem status and behavioral protocol (requires `WEND_RAG_MEMORY_ENABLED=true`). |
