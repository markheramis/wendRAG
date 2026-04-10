# Project Structure

```text
src/
  lib.rs                  Library entry point shared by the binary and tests
  main.rs                 Entry point: config, storage init, and MCP transport selection
  config.rs               Environment-based configuration
  mcp/
    server.rs             MCP tool handlers (WendRagServer)
    tools.rs              Tool input and output types
  ingest/
    reader.rs             Source detection and local-file readers
    url.rs                URL fetch, robots.txt checks, and HTML-to-markdown conversion
    chunker.rs            Structural and semantic chunking
    pipeline.rs           Ingest pipeline
  retrieve/
    dense.rs              Backend-agnostic dense search wrapper
    sparse.rs             Backend-agnostic sparse search wrapper
    fusion.rs             Reciprocal Rank Fusion
  embed/
    provider.rs           Embedding provider trait
    openai_compat.rs      OpenAI / Voyage / compatible implementation
  entity/
    mod.rs                Entity extraction client, graph models, and aggregation helpers
  store/
    models.rs             Shared database models
    mod.rs                StorageBackend trait and backend initialization
    postgres.rs           PostgreSQL backend implementation
    sqlite.rs             SQLite backend implementation

migrations/
  postgres/
    001_initial.sql
    002_embedding_1024.sql
    003_entity_graph.sql
  sqlite/
    001_initial.sql
    002_entity_graph.sql

docs/
  configuration.md        Full environment variable reference
  cli.md                  CLI flags and one-shot ingestion
  mcp-tools.md            MCP tool reference
  retrieval.md            Retrieval modes, graph expansion, and chunking
  project-structure.md    This file

Dockerfile
compose.yml
Cargo.toml
```

## Key Abstractions

- **`StorageBackend` trait** (`src/store/mod.rs`) — implemented by both `PostgresBackend` and `SqliteBackend`. All MCP handlers and retrieval code work against this trait, keeping the two backends interchangeable.
- **`EmbeddingProvider` trait** (`src/embed/provider.rs`) — implemented by `OpenAiCompatProvider`, which covers OpenAI, Voyage, and any OpenAI-compatible endpoint (including local Ollama).
- **`EntityExtractor` trait** (`src/entity/mod.rs`) — implemented by `OpenAiCompatEntityExtractor`. Used optionally during ingestion when `ENTITY_EXTRACTION_ENABLED=true`.
- **Ingest pipeline** (`src/ingest/pipeline.rs`) — coordinates reading, chunking, embedding, entity extraction, and storage in a single pass.
