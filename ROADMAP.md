# Code-RAG Development Roadmap

> Living document — last updated 2026-04-10.
> Based on competitive analysis of 14 MCP RAG implementations (see `resources/docs/comparison/`).

---

## Status Legend

| Badge | Meaning |
|-------|---------|
| `[DONE]` | Fully implemented and shipped |
| `[PARTIAL]` | Partially implemented — see notes |
| `[TODO]` | Not yet started |

---

## Guiding Principles

- **PostgreSQL + pgvector stays the primary backend.** All new features land here first.
- **Self-hosted and single-server.** No managed services, no multi-engine sprawl.
- **Correctness → Security → Performance → Maintainability.** In that order.
- **Smallest coherent patch.** Each phase should be independently shippable and useful.

---

## Phase 1 — Foundation Expansion (High Priority)

These items address the two biggest capability gaps identified in the competitive analysis
and lay the groundwork for everything that follows.

### 1.1 SQLite + sqlite-vec Local Backend — `[DONE]`

**Goal:** Offer a zero-infrastructure alternative so users who want a purely local,
single-file deployment can run code-rag without PostgreSQL.

**Why now:** Four of the fourteen competitors already ship SQLite backends
(mcp-memory-service, rag-memory-mcp, mcp-rag-server, context-portal).
Self-hosted users frequently ask for "no external DB" options.

**Scope:**

- Introduce a `StorageBackend` trait abstracting all repository operations
  (upsert, search_dense, search_sparse, delete, list).
- Implement `PostgresBackend` by refactoring the existing `repo.rs` behind the trait.
- Implement `SqliteBackend` using `rusqlite` (or `sqlx` with SQLite feature)
  plus the `sqlite-vec` extension for vector operations.
- SQLite sparse retrieval: use FTS5 for full-text search (replaces `tsvector`/`ts_rank`),
  trigram similarity via a custom tokenizer or application-side filtering.
- Backend selection via `STORAGE_BACKEND=postgres|sqlite` environment variable,
  with SQLite as the default when `DATABASE_URL` is absent.
- SQLite file path configurable via `SQLITE_PATH` (defaults to `./code-rag.db`).
- Write SQLite-equivalent migrations matching the PostgreSQL schema.

**Implementation notes:**
- `src/store/sqlite.rs` — FTS5 with Porter stemming + trigram fuzzy search (Sorensen-Dice)
- `src/store/postgres.rs` — pgvector HNSW + tsvector GIN full-text
- `StorageBackend` trait in `src/store/mod.rs`
- Migrations in `migrations/sqlite/` and `migrations/postgres/`

**Validation:**

- All existing MCP tools work identically on both backends.
- Hybrid retrieval (dense + sparse + RRF fusion) produces comparable results.
- Integration tests run against both backends in CI.

---

### 1.2 Graph / Entity-Aware Retrieval (PostgreSQL-Native) — `[DONE]`

**Goal:** Enrich retrieval with entity and relationship context so queries about
connected concepts surface relevant chunks beyond raw vector similarity.

**Why now:** Three competitors implement graph-aware retrieval (ApeRAG, rag-memory-mcp,
mcp-lightrag). This is the single most impactful retrieval quality improvement
identified in the analysis.

**Key finding from research:** This can be implemented entirely within PostgreSQL +
pgvector — no Neo4j or dedicated graph database required. The approach uses standard
relational tables for entities and relationships, recursive CTEs for graph traversal,
and pgvector for entity embeddings.

**Schema additions (new migration):**

```sql
/** Extracted named entities from chunk content. */
CREATE TABLE entities (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name            TEXT NOT NULL,
    entity_type     VARCHAR(50) NOT NULL,   -- PERSON, ORG, CONCEPT, LOCATION, etc.
    description     TEXT,
    embedding       vector(1024),           -- Entity-level embedding
    mention_count   INTEGER DEFAULT 1,
    created_at      TIMESTAMPTZ DEFAULT now(),
    UNIQUE(name, entity_type)
);

/** Join table: which entities appear in which chunks. */
CREATE TABLE entity_mentions (
    chunk_id    UUID NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
    entity_id   UUID NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    PRIMARY KEY (chunk_id, entity_id)
);

/** Directed edges between entities. */
CREATE TABLE entity_relationships (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    source_entity_id    UUID NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    target_entity_id    UUID NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    relationship_type   VARCHAR(100) NOT NULL,
    description         TEXT,
    weight              FLOAT DEFAULT 1.0,
    evidence_chunk_id   UUID REFERENCES chunks(id) ON DELETE SET NULL,
    created_at          TIMESTAMPTZ DEFAULT now()
);

CREATE INDEX idx_entities_embedding ON entities
    USING hnsw (embedding vector_cosine_ops);
CREATE INDEX idx_entities_type ON entities(entity_type);
CREATE INDEX idx_entity_mentions_chunk ON entity_mentions(chunk_id);
CREATE INDEX idx_entity_mentions_entity ON entity_mentions(entity_id);
CREATE INDEX idx_relationships_source ON entity_relationships(source_entity_id);
CREATE INDEX idx_relationships_target ON entity_relationships(target_entity_id);
```

**Retrieval pipeline — graph-boosted hybrid search:**

1. Standard hybrid search (dense + sparse + RRF) → initial ranked chunks.
2. From those chunks, resolve mentioned entities via `entity_mentions`.
3. Recursive CTE traversal (1–2 hops) over `entity_relationships` → related entities.
4. Retrieve additional chunks that mention the related entities.
5. Score graph-discovered chunks with a decay factor per hop distance.
6. Merge into the RRF result set as a third fusion branch (vector + sparse + graph).

**Entity extraction pipeline (during ingestion):**

- After chunking and embedding, pass chunk content through an LLM-based entity
  extraction step (configurable, off by default).
- Extract: entity name, entity type, relationships to other entities in the same chunk.
- Deduplicate and merge entities across chunks (by normalized name + type).
- Embed entity descriptions for entity-level similarity search.
- Provider: use the same embedding provider already configured; entity extraction
  uses a configurable LLM endpoint (OpenAI-compatible).

**Configuration:**

- `ENTITY_EXTRACTION_ENABLED=true|false` (default: false).
- `ENTITY_EXTRACTION_LLM_URL` and `ENTITY_EXTRACTION_LLM_MODEL` for the extraction model.
- `GRAPH_RETRIEVAL_ENABLED=true|false` (default: false, requires entities to exist).
- `GRAPH_TRAVERSAL_DEPTH=1|2|3` (default: 2).

**SQLite parity:** The same entity/relationship schema works in SQLite with
recursive CTEs. Entity embeddings use sqlite-vec. Implement alongside 1.1.

**Implementation notes:**
- `src/entity/mod.rs` — `OpenAiCompatEntityExtractor`, entity dedup and relationship insert
- `src/retrieve/hybrid.rs` — graph expansion fused as a third RRF branch
- `migrations/postgres/003_entity_graph.sql`, `migrations/sqlite/002_entity_graph.sql`
- Entity types: PERSON, ORG, CONCEPT, LOCATION, TECHNOLOGY, SERVICE, TEAM

**Validation:**

- Retrieval quality comparison: hybrid-only vs hybrid+graph on a test corpus.
- Graph traversal query latency benchmarks (must stay under 100ms for 2-hop).
- Entity extraction accuracy spot-checks on sample documents.

---

### 1.3 MCP Resources Pattern — `[DONE]`

**Goal:** Expose read-only state via MCP `resources` (not just tools),
giving clients richer introspection without tool-call overhead.

**Why now:** Identified as ADOPT-tier in the competitive analysis.
mcp-rag-server demonstrates this well with `rag://` URIs.

**Resources to expose:**

- `rag://status` — server health, document count, chunk count, index stats.
- `rag://documents` — paginated document list with metadata.
- `rag://documents/{id}` — single document detail with chunk summaries.
- `rag://config` — current server configuration (redacted secrets).

**Implementation notes:**
- All four URIs implemented in `src/mcp/server.rs`

**Validation:**

- MCP resource URIs resolve correctly over both HTTP and STDIO transports.
- Clients (Claude Desktop, etc.) can browse resources without tool calls.

---

### 1.4 OpenTelemetry Instrumentation — `[PARTIAL]`

**Goal:** Add structured observability to the ingestion and retrieval pipelines.

**Why now:** Identified as ADOPT-tier. ApeRAG demonstrates the value.
The Rust `tracing` crate (already a dependency) integrates cleanly with
`tracing-opentelemetry`.

**Instrumented spans:**

- Ingestion: file read → chunking → embedding API call → DB upsert.
- Retrieval: query embedding → dense search → sparse search → fusion → response.
- Entity extraction (Phase 1.2): LLM call → entity dedup → relationship insert.

**Configuration:**

- `OTEL_EXPORTER_OTLP_ENDPOINT` to enable (disabled when absent).
- Compatible with Jaeger, Grafana Tempo, or any OTLP collector.

**What's done:**
- `tracing` crate is wired up with structured `info!`/`warn!`/`error!` logging at key points
  (server startup, ingestion batches, chunking, MCP request handling)
- Log level controlled via `RUST_LOG`; logs go to stderr, JSON-RPC to stdout

**What remains:**
- `tracing-opentelemetry` integration — no OTLP exporter, no named spans, no metrics
- `OTEL_EXPORTER_OTLP_ENDPOINT` env var not yet wired
- Pipeline spans (embed API call timing, DB upsert, fusion latency) not instrumented

---

### 1.5 Incremental Sync Refinement — `[DONE]`

**Goal:** Make re-ingestion of directories smarter and more explicit.

**Why now:** Identified as ADOPT-tier. mcp-lightrag demonstrates clean
content-hash-based sync semantics.

**Improvements:**

- Directory ingest returns a sync report: added, updated, unchanged, deleted counts.
- Optional `delete_removed=true` flag to remove documents whose source files
  no longer exist in the directory.
- Expose sync status per document via MCP resources (Phase 1.3).

**Implementation notes:**
- SHA-256 `content_hash` on every document; status per doc: `Created`, `Updated`, `Unchanged`
- `IngestPathResult` struct aggregates `added`, `updated`, `unchanged`, `deleted`, `failed`
- CLI mode outputs JSON sync report to stdout; MCP tool returns structured results

---

## Phase 2 — Retrieval Quality & Ingestion Breadth

These items improve what goes in and what comes out, building on the foundation.

### 2.1 HTML / URL Ingestion — `[DONE]`

**Goal:** Ingest web pages as documents using readability-style extraction.

**Evidence:** mcp-local-rag implements this with Mozilla Readability → Turndown →
markdown. py-mcp-qdrant-rag attempted it but shipped a placeholder.

**Approach:**

- New file type: `url`.
- Fetch URL content → extract readable content (Rust: `readability` or
  `scraper` crate) → convert to markdown → feed into existing chunking pipeline.
- Store original URL as `file_path` for dedup and re-fetch.
- Respect robots.txt and rate limits.

**Implementation notes:**
- `src/ingest/url.rs` — HTTP fetch with 30s timeout, robots.txt enforcement, 429/Retry-After handling
- HTML → Markdown via `html-to-markdown-rs`
- File type `url` stored in DB; migrations in `migrations/*/0{03,04}_url_file_type.sql`
- User-Agent: `wend-rag/0.1`

### 2.2 Ollama / Local Embedding Support — `[PARTIAL]`

**Goal:** First-class support for fully local embeddings via Ollama.

**Evidence:** Five competitors use Ollama successfully. Code-rag already supports
OpenAI-compatible endpoints, so Ollama partially works today.

**Improvements:**

- Add `ollama` as a named embedding provider with sensible defaults
  (model: `nomic-embed-text`, dimensions: 768, base URL: `http://localhost:11434`).
- Handle Ollama's specific API shape if it diverges from OpenAI-compatible.
- Document the setup and model recommendations.
- Adjust vector column dimension dynamically or via migration helper.

**What's done:**
- `EMBEDDING_PROVIDER=openai-compatible` with custom `EMBEDDING_BASE_URL` already works with Ollama

**What remains:**
- Named `ollama` provider with hardcoded sensible defaults (model, dims, base URL)
- Explicit dimension migration helper for switching between embedding sizes
- Setup documentation and model recommendations

### 2.3 Reranking Stage — `[TODO]`

**Goal:** Add an optional reranking step after fusion to improve precision.

**Evidence:** minima implements reranking effectively. Cross-encoder reranking
consistently improves retrieval quality in benchmarks.

**Approach:**

- After RRF fusion, pass top-N candidates through a reranker.
- Support Cohere Rerank API, Jina Reranker, or local cross-encoder via
  OpenAI-compatible endpoint.
- Configuration: `RERANKER_ENABLED`, `RERANKER_PROVIDER`, `RERANKER_MODEL`.

### 2.4 Broader File Format Support — `[PARTIAL]`

**Goal:** Ingest DOCX, CSV, and JSON files in addition to markdown, text, and PDF.

**Evidence:** mcp-rag-server supports JSON/CSV; py-mcp-qdrant-rag supports DOCX.

**Approach:**

- Add readers in `ingest/reader.rs` for each format.
- DOCX: extract text and headings (Rust `docx-rs` or similar).
- CSV/JSON: convert rows/objects to text chunks with structural context.

**What's done:**
- Markdown, plain text, PDF, and URL already supported

**What remains:**
- DOCX reader
- CSV reader (rows → text chunks with structural context)
- JSON reader (objects → text chunks)

---

## Phase 3 — Advanced Retrieval & Intelligence

### 3.1 Entity Communities and Hierarchical Context — `[TODO]`

**Goal:** Extend graph retrieval (Phase 1.2) with community detection for
global-context queries, following Microsoft GraphRAG's two-tier approach.

**Approach:**

- After entity extraction, run community detection (Louvain or label propagation)
  on the entity relationship graph.
- Generate LLM-summarized descriptions per community.
- Embed community summaries for community-level similarity search.
- Two retrieval modes:
  - **Local:** chunk-level search + entity graph traversal (Phase 1.2).
  - **Global:** community-level search for broad, thematic queries.

**Schema addition:**

```sql
CREATE TABLE entity_communities (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name            TEXT,
    summary         TEXT,
    tier            INTEGER DEFAULT 1,
    entity_ids      UUID[],
    embedding       vector(1024),
    importance      FLOAT DEFAULT 0.0,
    created_at      TIMESTAMPTZ DEFAULT now()
);
```

### 3.2 Semantic Chunking Improvements — `[PARTIAL]`

**Goal:** Improve the existing semantic chunking strategy based on findings
from mcp-local-rag's Max-Min algorithm.

**Improvements:**

- Hard similarity thresholds with forced splits at configurable sentence counts.
- Garbage/boilerplate filtering before chunking.
- Benchmark against current fixed-window strategy on real corpora.

**What's done:**
- Semantic chunking exists (`CHUNKING_STRATEGY=semantic`) using embedding similarity
  with configurable percentile threshold (`CHUNKING_SEMANTIC_THRESHOLD`, default 0.25)
- `split_oversized()` handles chunks that exceed max size after semantic splitting

**What remains:**
- Forced splits at configurable max sentence counts (Max-Min hard boundary)
- Garbage/boilerplate pre-filtering
- Benchmarks comparing fixed vs semantic on real corpora

### 3.3 Query Routing — `[TODO]`

**Goal:** Automatically select retrieval strategy based on query characteristics.

**Approach:**

- Classify queries as local (specific, factual) vs global (thematic, exploratory).
- Local queries → standard hybrid + entity graph.
- Global queries → community-level retrieval + broader context.
- Classification can be rule-based initially, LLM-based later.

---

## Phase 4 — Ecosystem & Operational Maturity

### 4.1 Additional Vector Backend Options (Future) — `[TODO]`

**Purpose:** Evaluate alternatives if PostgreSQL + pgvector hits scaling limits.

**Candidates to watch:**

- **Qdrant:** Sophisticated hybrid pipelines, native RRF, multivector support.
  Three competitors use it (ApeRAG, minima, py-mcp-qdrant-rag). Consider if
  vector index performance becomes a bottleneck or advanced features are needed.
- **LanceDB:** Local-first, columnar, good for embedded deployments. Could
  complement SQLite backend for vector-heavy local workloads.
- **pgvectorscale / pgembedding:** PostgreSQL-native improvements to pgvector.
  Watch for maturity.

**Decision criteria:** Only adopt if PostgreSQL + pgvector demonstrably fails
to meet latency or scale requirements with proper indexing (HNSW, IVFFlat tuning).

### 4.2 Memory / Session Layer — `[TODO]`

**Goal:** Optional structured memory subsystem for agent-oriented use cases.

**Evidence:** mcp-memory-service and context-portal show demand for durable
agent memory with decay, consolidation, and session tagging.

**Approach:** Build as a separate module on top of the entity graph, not as
a replacement for corpus retrieval. Memory entries are documents with
special metadata and lifecycle rules.

### 4.3 Multi-Tenant / Workspace Isolation — `[TODO]`

**Goal:** Support multiple isolated workspaces within a single server instance.

**Approach:**

- Extend the existing `project` field to act as a full workspace boundary.
- Per-workspace entity graphs and community hierarchies.
- Optional per-workspace embedding configuration.

### 4.4 Dashboard / Admin UI — `[TODO]`

**Goal:** Lightweight web UI for monitoring ingestion, browsing documents,
and inspecting entity graphs.

**Approach:** Static SPA served by the existing Axum server. Read-only initially,
with ingestion triggers added later.

---

## Summary Matrix

| Item | Phase | Priority | Complexity | Depends On | Status |
|------|-------|----------|------------|------------|--------|
| SQLite + sqlite-vec backend | 1.1 | Critical | High | — | `[DONE]` |
| Graph / entity-aware retrieval | 1.2 | Critical | High | — | `[DONE]` |
| MCP resources pattern | 1.3 | High | Low | — | `[DONE]` |
| OpenTelemetry instrumentation | 1.4 | High | Low | — | `[PARTIAL]` |
| Incremental sync refinement | 1.5 | High | Low | — | `[DONE]` |
| HTML / URL ingestion | 2.1 | Medium | Medium | — | `[DONE]` |
| Ollama local embeddings | 2.2 | Medium | Low | — | `[PARTIAL]` |
| Reranking stage | 2.3 | Medium | Medium | — | `[TODO]` |
| Broader file formats | 2.4 | Medium | Medium | — | `[PARTIAL]` |
| Entity communities | 3.1 | Medium | High | 1.2 | `[TODO]` |
| Semantic chunking improvements | 3.2 | Medium | Medium | — | `[PARTIAL]` |
| Query routing | 3.3 | Medium | Medium | 1.2, 3.1 | `[TODO]` |
| Alternative vector backends | 4.1 | Low | High | 1.1 | `[TODO]` |
| Memory / session layer | 4.2 | Low | High | 1.2 | `[TODO]` |
| Multi-tenant workspaces | 4.3 | Low | Medium | — | `[TODO]` |
| Dashboard / admin UI | 4.4 | Low | Medium | 1.3 | `[TODO]` |

---

## What We Deliberately Skip

These were evaluated in the competitive analysis and rejected:

- **Proprietary managed backends** (GroundX, etc.) — breaks self-hosted requirement.
- **Multi-service platform sprawl** — conflicts with single-server design.
- **LLM-assisted chunking as default** — non-deterministic, hard to validate.
- **LangChain / heavy framework dependency** — we own our pipeline.
- **Full graph-first architecture** — graph enriches retrieval, not replaces it.
