# Project Structure

```text
src/
  lib.rs                    Library entry point shared by the binary and tests
  main.rs                   CLI, daemon wiring, Axum router, auth middleware
  config.rs                 Runtime Config (WEND_RAG_* + YAML merging)
  config_file.rs            YAML FileConfig loader
  auth.rs                   API key generation, SHA-256 hashing, keys.json store
  observability.rs          Tracing + optional OpenTelemetry OTLP export

  mcp/
    server.rs               WendRagServer + tool_router implementation
    server_resources.rs     rag://status, rag://documents, rag://communities, ...
    tools.rs                Tool input/output types + size-limit validators

  embed/
    provider.rs             EmbeddingProvider trait
    openai_compat.rs        OpenAI / Voyage / any OpenAI-compatible endpoint
    ollama.rs               Native Ollama client
    mod.rs                  Shared types

  ingest/
    mod.rs                  Module glue
    reader.rs               Source detection, safe-path validation, local-file readers
    url.rs                  URL fetch, SSRF guard (incl. DNS-level re-validation)
    chunker.rs              Structural chunking dispatcher
    chunker_sections.rs     Heading-based splitting
    chunker_semantic.rs     Embedding-similarity chunking
    directory.rs            Concurrent directory ingestion
    pipeline.rs             Single-document pipeline (hash, chunk, embed, extract, upsert)
    types.rs                Shared ingest types

  retrieve/
    mod.rs                  ScoredChunk, SearchMode
    dense.rs                Dense retrieval wrapper
    sparse.rs               Sparse retrieval wrapper
    hybrid.rs               RRF fusion of dense + sparse + graph + community
    fusion.rs               Reciprocal Rank Fusion implementation
    router.rs               Query classifier (local / global / hybrid)
    community.rs            Community retrieval branch

  rerank/
    provider.rs             RerankerProvider trait
    cohere.rs               Cohere rerank client
    jina.rs                 Jina rerank client
    openai_compat.rs        OpenAI-compatible rerank client
    mod.rs                  Reranker config + factory

  entity/
    mod.rs                  Entity trait + graph settings
    model.rs                Entity / relationship / graph structs
    extractor.rs            LLM-backed entity extractor (OpenAI-compatible)
    graph_build.rs          In-memory graph deduplication
    normalize.rs            Name normalisation
    community.rs            Louvain community detection
    community_manager.rs    Community summary generation + two-tier retrieval

  memory/
    mod.rs                  Module glue + decay helpers
    manager.rs              MemoryManager: session buffers, stores, decay
    buffer.rs               In-memory SessionBuffer with sliding window
    retrieval.rs            MemoryRetriever with recency-weighted scoring
    maintenance.rs          Background decay / pruning / session cleanup
    storage.rs              MemoryStorage trait
    pg_storage.rs           Postgres implementation (pgvector HNSW)
    sqlite_storage.rs       SQLite implementation (BLOB + Rust cosine)
    types.rs                MemoryEntry / MemoryQuery / MemoryContext

  store/
    mod.rs                  StorageBackend trait + backend initialisation
    models.rs               Shared database-facing models
    postgres/
      mod.rs                Postgres backend
      entity_graph.rs       Entity graph persistence (pgvector + batch inserts)
      community.rs          Community persistence (HNSW ANN)
      search.rs             Dense / sparse / hybrid SQL
    sqlite/
      mod.rs                SQLite backend
      entity_graph.rs       Entity graph persistence (batch inserts via QueryBuilder)
      community.rs          Community persistence (cosine in Rust)
      embeddings.rs         Blob encoding / decoding / cosine
      filters.rs            SearchFilters → SQL clauses
      mappers.rs            Row → model conversions
      text_util.rs          FTS5 escaping

migrations/
  postgres/
    001_initial.sql
    002_embedding_1024.sql
    003_entity_graph.sql
    004_url_file_type.sql
    005_entity_communities.sql
    006_memory.sql
    007_add_indexes.sql         Partial index on active memory_entries +
                                entities.normalized_name index
  sqlite/
    001_initial.sql
    002_entity_graph.sql
    003_url_file_type.sql
    004_entity_communities.sql
    005_memory.sql
    006_add_indexes.sql         Partial index on active memory_entries

resources/
  docs/                         User-facing Markdown documentation
    authentication-setup.md     API key generation, storage, and operator guide
    cli.md                      CLI flags, subcommands, and ingestion output
    configuration.md            Complete WEND_RAG_* reference
    entity-communities.md       Louvain + two-tier retrieval architecture
    mcp-client-setup.md         Cursor / Claude / VS Code / Codex / curl config
    mcp-tools.md                MCP tool reference + input size limits
    memory-sessions.md          Memory subsystem and session lifecycle
    project-structure.md        This file
    retrieval.md                Retrieval modes, chunking, RRF fusion
    setup-example.md            End-to-end EC2 + Postgres + Ollama walkthrough
  testing/
    README.md
    config.json

tests/
  auth_cli.rs                   CLI key:generate / key:list / key:revoke lifecycle
  backend_parity.rs             Postgres/SQLite behavioural parity
  daemon.rs                     Health endpoint, graceful shutdown, Bearer auth
  semantic_chunking_test.rs     Chunker behaviour on real documents

Cargo.toml
compose.yml
Dockerfile
config.example.yaml
README.md
```

## Key Abstractions

- **`StorageBackend` trait** (`src/store/mod.rs`) — implemented by both
  `PostgresBackend` and `SqliteBackend`. All MCP handlers and retrieval
  code work against this trait, keeping the two backends interchangeable.
- **`EmbeddingProvider` trait** (`src/embed/provider.rs`) — implemented by
  `OpenAiCompatProvider` (covers OpenAI, Voyage, and any OpenAI-compatible
  endpoint) and `OllamaProvider` (native Ollama client).
- **`EntityExtractor` trait** (`src/entity/mod.rs`) — implemented by
  `OpenAiCompatEntityExtractor`. Used optionally during ingestion when
  `WEND_RAG_ENTITY_EXTRACTION_ENABLED=true`.
- **`RerankerProvider` trait** (`src/rerank/provider.rs`) — implemented by
  Cohere, Jina, and OpenAI-compatible rerank clients. Enabled with
  `WEND_RAG_RERANKER_ENABLED=true`.
- **`MemoryStorage` trait** (`src/memory/storage.rs`) — implemented by
  `PostgresMemoryStorage` and `SqliteMemoryStorage`. Consumed by
  `MemoryManager` which is passed into `WendRagServer` as an
  `Arc<MemoryManager>`.
- **`Authenticator`** (`src/auth.rs`) — combines an optional
  `WEND_RAG_API_KEY` static key with the file-backed `KeyStore` for the
  Axum Bearer auth middleware.
- **Ingest pipeline** (`src/ingest/pipeline.rs`) — coordinates reading,
  chunking, embedding, concurrent entity extraction, and storage upsert
  in a single pass.
