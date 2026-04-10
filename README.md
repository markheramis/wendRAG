# wendRAG

An MCP server for markdown, text, PDF, and URL-backed HTML documents with hybrid dense + sparse retrieval, optional entity graph expansion, and dual PostgreSQL / SQLite storage backends.

## Features

- Ingest local files, HTTP(S) URLs, directories, and inline document batches.
- Hybrid dense + sparse retrieval with Reciprocal Rank Fusion.
- Optional entity-aware graph expansion on both backends.
- Seven MCP tools covering ingestion, search, listing, and deletion.
- OpenAI-compatible embeddings client — works with OpenAI, Voyage, or any local provider (e.g. Ollama).
- Fixed or semantic chunking for oversized content.
- Respects `robots.txt` before URL ingestion.

## Quick Start

### SQLite (zero infrastructure)

Prerequisites: Rust 1.85+.

```bash
cp .env.example .env
# Edit .env:
#   STORAGE_BACKEND=sqlite
#   EMBEDDING_PROVIDER, EMBEDDING_API_KEY, EMBEDDING_MODEL (must emit 1024-dimensional vectors)
cargo run
```

The SQLite database is created automatically and migrations run on startup. The MCP endpoint is available at `http://localhost:3000/mcp`.

### PostgreSQL

Prerequisites: Rust 1.85+ and a PostgreSQL instance with the `pgvector` and `pg_trgm` extensions.

```bash
cp .env.example .env
# Edit .env:
#   STORAGE_BACKEND=postgres
#   DATABASE_URL=postgres://user:pass@host:5432/dbname
#   EMBEDDING_PROVIDER, EMBEDDING_API_KEY, EMBEDDING_MODEL
cargo run
```

Migrations run automatically on startup. The MCP endpoint is available at `http://localhost:3000/mcp`.

## Further Reading

| Topic | File |
|---|---|
| Full environment variable reference | [docs/configuration.md](docs/configuration.md) |
| CLI flags and one-shot ingestion | [docs/cli.md](docs/cli.md) |
| MCP tool reference | [docs/mcp-tools.md](docs/mcp-tools.md) |
| Retrieval modes, graph expansion, chunking | [docs/retrieval.md](docs/retrieval.md) |
| Source layout and key abstractions | [docs/project-structure.md](docs/project-structure.md) |

## License

MIT
