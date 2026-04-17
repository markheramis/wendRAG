# Changelog

All notable changes to **wendRAG** are documented in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2026-04-17

Consolidated release covering audit remediation, API key authentication, chunk
retrieval, performance work, and documentation overhaul. All development
between commit `b043f55` and this entry is rolled up under this version.

### Added

- API key authentication for the HTTP transport with a new `Authenticator`,
  SHA-256 key hashing, and a file-backed `KeyStore`.
- CLI subcommands `key:generate`, `key:list`, `key:revoke` for managing keys
  without touching the database.
- `WEND_RAG_API_KEY` env var for a single static Bearer token and
  `WEND_RAG_KEYS_FILE` to override the keys file path.
- New MCP tool `rag_get_chunk` for fetching a specific chunk (with optional
  `before` / `after` neighbour window) by `file_path` or `document_id`.
- `StorageBackend::get_chunks_by_index` on both Postgres and SQLite.
- Server-side input size limits (1 MiB content, 10 KiB query, 100-item batch)
  enforced at every MCP tool handler.
- New documentation: `authentication-setup.md`, `mcp-client-setup.md`.
- Migrations: `migrations/postgres/007_add_indexes.sql`,
  `migrations/sqlite/006_add_indexes.sql` (partial index on active
  `memory_entries` + `entities.normalized_name` index on Postgres).
- Integration test suites: `tests/auth_cli.rs` and the Bearer-auth case in
  `tests/daemon.rs`; new parity test `backends_return_chunks_by_index`.

### Changed

- URL ingestion now installs a custom `reqwest` DNS resolver that
  re-validates every resolved IP, closing the DNS rebinding TOCTOU window.
- `rag_ingest_batch` and related handlers reject requests above the new size
  caps with structured JSON errors.
- SQLite chunk, entity-mention, entity-relationship, and community-member
  inserts now use `QueryBuilder::push_values` batching.
- Entity extraction runs up to 4 concurrent LLM calls via
  `futures::stream::buffered`.
- Graph and community search branches now execute concurrently with
  `tokio::join!`.
- `sha256_hex` pre-allocates a single 64-byte `String` instead of one
  `String` per byte.
- `CommunityManager` reuses a single `reqwest::Client` across summary calls.
- `IngestOptions` is passed through directory ingestion inside an `Arc`.
- `StorageBackend::replace_document_entity_graph` now takes `&mut
  DocumentEntityGraph` so the Postgres backend can move embeddings instead
  of cloning them.
- Every `WEND_RAG_*` env var is now the single source of truth; docs no
  longer reference unprefixed legacy names.
- Every file-level `/** … */` doc comment converted to inner `/*! … */`.
- `RerankerProviderKind::from_str` renamed to `parse` to avoid confusion
  with `std::str::FromStr`.
- `README.md` and `resources/docs/*` rewritten to reflect the `WEND_RAG_`
  prefix, the auth layer, the memory subsystem, and the updated project
  structure.

### Removed

- MCP tool `rag_get_full_context` and the `src/mcp/reconstruct.rs` module
  it relied on.
- Dead memory-subsystem code: `MemoryManager::is_enabled` / `config` /
  `add_session_message` / `summarize_session` / `get_memory` /
  `build_context` / `with_session_buffer`, `MemoryConfig::minimal`,
  `SessionBuffer::get_all_messages` / `clear` / `set_summary`,
  `MemoryQuery::min_importance` / `entry_type`, and
  `build_memory_context`.
- Superseded convenience wrappers `read_source` and `read_url_document`
  (callers now use the `_with_options` variants directly).
- `#[allow(dead_code)]` graph-introspection stubs on `SparseGraph`.

### Fixed

- **SEC-01:** `/mcp` is now gated by Bearer auth when any key is
  configured. `/health` remains unauthenticated for probes.
- **SEC-02a:** DNS rebinding closed by the re-validating resolver.
- **SEC-02b:** IPv4-mapped IPv6 addresses (e.g. `::ffff:127.0.0.1`) are
  now treated as loopback/private.
- **SEC-02c:** Decimal-encoded IPv4 (e.g. `2130706433`) is recognised and
  blocklisted even when the URL parser classifies it as a domain.
- **SEC-03:** Unbounded input sizes rejected before any embedding call.
- **SEC-04:** Path-traversal TOCTOU removed — `validate_safe_path` now
  requires successful canonicalisation.
- **SEC-05:** Windows UNC paths rejected up front.

### Performance

- **PERF-01:** Parallel entity extraction (4-way).
- **PERF-02:** Batched SQLite inserts for chunks / mentions / relationships
  / community members.
- **PERF-03:** Embedding `Vec<f32>` moved into the Postgres pgvector
  binding instead of cloned per-entity.
- **PERF-05:** Parallel graph + community retrieval branches.
- **PERF-06:** `sha256_hex` single-allocation rewrite.
- **PERF-07:** Shared `reqwest::Client` in `CommunityManager`.
- **PERF-08:** `Arc<IngestOptions>` in directory ingestion.
- **PERF-09:** Partial index on active `memory_entries` (Postgres +
  SQLite) and `entities.normalized_name` index on Postgres.

### Tests

- Grew from 85 → **133 passing tests** (0 failed, 0 ignored) across unit,
  backend parity, HTTP daemon, CLI, and semantic chunking suites.
- Zero `cargo clippy --all-targets -- -D warnings` findings.

---

## [0.1.0] - 2026-04-16

Initial feature-complete release squashed from all prior commits.

### Added

- Core MCP server over Streamable HTTP (`/mcp`) and stdio transports.
- Document ingestion pipeline for markdown, text, PDF, DOCX, CSV, JSON, and
  HTTP(S) URLs, with `robots.txt` enforcement and an initial SSRF guard.
- Dual-backend storage: PostgreSQL + pgvector, and SQLite with in-Rust
  cosine similarity.
- Hybrid retrieval (dense + sparse) with Reciprocal Rank Fusion.
- Optional entity extraction, entity graph persistence, and graph-augmented
  retrieval.
- Louvain-based entity communities with two-tier local / global retrieval
  and optional LLM-generated summaries.
- Query router (local / global / hybrid classification) for automatic
  retrieval strategy selection.
- Memory subsystem with session buffers, persistent user / global memories,
  decay + pruning maintenance task, and MCP tools (`memory_store`,
  `memory_retrieve`, `memory_forget`, `memory_sessions`).
- Optional reranking stage (Cohere, Jina, OpenAI-compatible).
- Fixed and semantic chunking strategies with garbage filtering.
- YAML + `WEND_RAG_*` environment configuration with merge precedence.
- Connection pool configuration shared by both backends.
- CLI subcommands `daemon`, `stdio`, `ingest`.
- OpenTelemetry OTLP tracing export and `/health` endpoint with graceful
  shutdown on SIGTERM / Ctrl-C.
- Docker Compose setup and a full EC2 + PostgreSQL + Ollama setup guide.

[0.2.0]: https://semver.org/spec/v2.0.0.html
[0.1.0]: https://semver.org/spec/v2.0.0.html
