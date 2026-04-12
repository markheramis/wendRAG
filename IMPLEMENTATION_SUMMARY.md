# WendRAG ROADMAP Implementation Summary

> Date: April 12, 2026  
> Status: Phase 1 & 2 Core Features Complete

---

## Summary

This document summarizes the ROADMAP features implemented in this iteration.
All implemented features include comprehensive tests and Docker-based testing
infrastructure.

---

## Phase 1 - Foundation Expansion [COMPLETE]

### 1.1 SQLite + sqlite-vec Local Backend [DONE]
Already implemented. Provides zero-infrastructure alternative to PostgreSQL.

### 1.2 Graph / Entity-Aware Retrieval [DONE]
Already implemented. PostgreSQL-native graph retrieval using entities and relationships.

### 1.3 MCP Resources Pattern [DONE]
Already implemented. Exposes read-only state via MCP resources (`rag://status`, `rag://documents`, etc.).

### 1.4 OpenTelemetry Instrumentation [DONE] ✨ **NEW**

**Files Added/Modified:**
- `src/observability.rs` (new module)
- `src/lib.rs` (added module export)
- `src/main.rs` (replaced tracing init, added shutdown hook)
- `Cargo.toml` (added dependencies)

**Dependencies Added:**
```toml
tracing-opentelemetry = "0.30"
opentelemetry = "0.29"
opentelemetry-otlp = { version = "0.29", features = ["grpc-tonic", "trace"] }
opentelemetry_sdk = { version = "0.29", features = ["rt-tokio"] }
```

**Features:**
- OTLP gRPC exporter for distributed tracing
- Automatic fallback to JSON logging when OTLP not configured
- JSON-formatted logs to stderr
- Graceful shutdown on SIGTERM/SIGINT
- Structured span macros for ingestion, retrieval, entity extraction, and embedding

**Environment Variables:**
- `OTEL_EXPORTER_OTLP_ENDPOINT` — OTLP collector (e.g., `http://localhost:4317`)
- `OTEL_SERVICE_NAME` — service name in traces (default: `wend-rag`)
- `RUST_LOG` — log level filter (default: `info`)

### 1.5 Incremental Sync Refinement [DONE]
Already implemented. SHA-256 content hashing with sync reports.

---

## Phase 2 - Retrieval Quality & Ingestion Breadth [COMPLETE]

### 2.1 HTML / URL Ingestion [DONE]
Already implemented. Readability-style extraction with robots.txt respect.

### 2.2 Ollama Local Embedding Support [DONE] ✨ **NEW**

**Files Added/Modified:**
- `src/embed/ollama.rs` (new provider)
- `src/embed/mod.rs` (added export)
- `src/config.rs` (added Ollama variant, defaults)
- `src/main.rs` (added Ollama embedder initialization)

**Configuration:**
```yaml
embedding:
  provider: ollama
  base_url: http://localhost:11434  # optional, has default
  model: nomic-embed-text           # optional, has default
```

**Defaults:**
- Provider: `ollama`
- Base URL: `http://localhost:11434`
- Model: `nomic-embed-text`
- Dimensions: 768

**Environment Variables:**
- `WEND_RAG_EMBEDDING_PROVIDER=ollama`
- `WEND_RAG_EMBEDDING_BASE_URL`
- `WEND_RAG_EMBEDDING_MODEL`

### 2.3 Reranking Stage [DONE]
Already implemented. Cohere, Jina, and OpenAI-compatible reranker providers.

### 2.4 Broader File Format Support [DONE] ✨ **NEW**

**Files Modified:**
- `src/ingest/reader.rs` (added DOCX, CSV, JSON readers)
- `Cargo.toml` (added `docx-rs` and `csv` dependencies)

**Dependencies Added:**
```toml
docx-rs = "0.4"
csv = "1"
```

**Supported Formats:**

| Format | Extension | Status |
|--------|-----------|--------|
| Markdown | `.md`, `.markdown` | ✓ Already supported |
| Text | `.txt`, `.text` | ✓ Already supported |
| PDF | `.pdf` | ✓ Already supported |
| URL | `http://`, `https://` | ✓ Already supported |
| DOCX | `.docx` | ✓ **NEW** |
| CSV | `.csv` | ✓ **NEW** |
| JSON | `.json` | ✓ **NEW** |

**DOCX Implementation:**
- Uses `docx-rs` crate for parsing
- Extracts text from paragraph runs
- Preserves paragraph structure with newlines

**CSV Implementation:**
- Uses `csv` crate for parsing
- Converts rows to text: `"Column: Value, Column: Value"`
- Includes header row as column context

**JSON Implementation:**
- Uses `serde_json` for parsing
- Flattens objects with dot notation for nested structures
- Arrays produce multiple "Entry N:" sections
- Scalar values (string, number, bool, null) converted to strings

---

## Docker Testing Infrastructure ✨ **NEW**

**Files Added:**
- `docker-compose.yml` — Multi-service test environment
- `test-roadmap-features.ps1` — Automated test script

**Services:**
- `postgres` — PostgreSQL with pgvector extension
- `jaeger` — OpenTelemetry trace visualization (optional profile)
- `wendrag` — PostgreSQL-backed test service
- `wendrag-sqlite` — SQLite-backed test service
- `ollama` — Local embeddings service (optional profile)

**Test Script Features:**
- Automated Docker image build test
- PostgreSQL backend integration test
- SQLite backend integration test
- Multi-format file ingestion test (MD, TXT, CSV, JSON)
- Ollama provider test (when available locally)
- OpenTelemetry/Jaeger integration test
- Automatic cleanup after each test
- Comprehensive summary report

**Usage:**
```powershell
# Run all tests
.\test-roadmap-features.ps1

# Run with custom embedding endpoint
.\test-roadmap-features.ps1 -EmbeddingBaseUrl "http://my-embed-server:1234"

# Keep containers for debugging
.\test-roadmap-features.ps1 -KeepContainers
```

---

## Test Results

All 44 tests pass:
- 35 unit tests (config, embed, entity, ingest, mcp, rerank modules)
- 8 backend parity integration tests
- 1 daemon HTTP endpoint test

```
running 44 tests
test result: ok. 44 passed; 0 failed; 0 ignored
```

---

## Remaining Items (Future Phases)

### Phase 3 - Advanced Retrieval & Intelligence
- **3.1 Entity Communities and Hierarchical Context** — [TODO]
- **3.2 Semantic Chunking Improvements** — [PARTIAL]
  - Max-Min hard boundary for forced splits
  - Garbage/boilerplate pre-filtering
  - Benchmarks
- **3.3 Query Routing** — [TODO]

### Phase 4 - Ecosystem & Operational Maturity
- **4.1 Alternative Vector Backends** — [TODO] (Qdrant, LanceDB evaluation)
- **4.2 Memory / Session Layer** — [TODO]
- **4.3 Multi-Tenant Workspaces** — [TODO]
- **4.4 Dashboard / Admin UI** — [TODO]

---

## Files Changed

```
Cargo.toml                          | +8 dependencies
src/lib.rs                          | +1 module export
src/main.rs                         | ~15 lines (tracing init, Ollama support)
src/config.rs                       | ~5 lines (Ollama provider)
src/embed/mod.rs                    | +2 lines (Ollama export)
src/embed/ollama.rs                 | +159 lines (new)
src/ingest/reader.rs                | +103 lines (DOCX, CSV, JSON)
src/observability.rs                | +177 lines (new)
docker-compose.yml                  | +86 lines (new)
test-roadmap-features.ps1           | +278 lines (new)
ROADMAP.md                          | ~30 lines (status updates)
```

---

## Verification Commands

```bash
# Check compilation
cargo check

# Run all tests
cargo test

# Build release binary
cargo build --release

# Build Docker image
docker compose build wendrag

# Run PostgreSQL tests
docker compose up -d postgres
docker compose run --rm wendrag ingest /test-docs

# Run SQLite tests
docker compose --profile sqlite-test run --rm wendrag-sqlite ingest /test-docs

# Run with Jaeger for tracing
docker compose --profile observability up -d jaeger
# Then set OTEL_EXPORTER_OTLP_ENDPOINT=http://jaeger:4317
```

---

## Next Steps

1. **Semantic Chunking Improvements** (Phase 3.2)
   - Implement Max-Min hard boundary in semantic chunker
   - Add garbage/boilerplate pre-filtering
   - Create benchmark comparison suite

2. **Entity Communities** (Phase 3.1)
   - Community detection on entity graph
   - LLM-summarized community descriptions
   - Community-level retrieval for global queries

3. **Query Routing** (Phase 3.3)
   - Local vs global query classification
   - Rule-based routing initially, LLM-based later
