# wendRAG

An MCP server for markdown, text, PDF, DOCX, CSV, JSON, and URL-backed
HTML documents with hybrid dense + sparse retrieval, entity-graph and
community expansion, agent memory, and dual PostgreSQL / SQLite storage
backends.

## Features

- Ingest local files, HTTP(S) URLs, directories, and inline document
  batches.
- Hybrid dense + sparse retrieval with Reciprocal Rank Fusion, plus
  optional graph and community branches.
- Optional API key authentication (`Authorization: Bearer ...`) on the
  HTTP transport, managed via `wend-rag key:generate / key:list /
  key:revoke`.
- Optional agent-oriented memory subsystem (`memory_store`,
  `memory_retrieve`, `memory_forget`, `memory_sessions`).
- OpenAI-compatible embeddings client — works with OpenAI, Voyage,
  Ollama, or any OpenAI-compatible endpoint.
- Optional reranker stage (Cohere, Jina, or any OpenAI-compatible
  rerank endpoint).
- Fixed or semantic chunking for oversized content.
- Defence in depth on URL ingestion: `robots.txt` enforcement, SSRF
  blocklist (IPv4, IPv6, IPv4-mapped IPv6, decimal-encoded IPs) and a
  DNS-level re-validating resolver that closes the DNS rebinding TOCTOU
  window.
- Server-side input size caps (1 MiB content, 10 KiB query, 100-item
  batches) enforced before any embedding work runs.

## Quick Start

### SQLite (zero infrastructure)

Prerequisites: Rust 1.85+.

```bash
cp .env.example .env
# Edit .env:
#   WEND_RAG_STORAGE_BACKEND=sqlite
#   WEND_RAG_EMBEDDING_PROVIDER, WEND_RAG_EMBEDDING_API_KEY,
#   WEND_RAG_EMBEDDING_MODEL (must emit 1024-dimensional vectors)
cargo run -- daemon
```

The SQLite database is created automatically and migrations run on
startup. The MCP endpoint is available at `http://localhost:3000/mcp`.

### PostgreSQL

Prerequisites: Rust 1.85+ and a PostgreSQL instance with the
`pgvector` and `pg_trgm` extensions.

```bash
cp .env.example .env
# Edit .env:
#   WEND_RAG_STORAGE_BACKEND=postgres
#   WEND_RAG_DATABASE_URL=postgres://user:pass@host:5432/dbname
#   WEND_RAG_EMBEDDING_PROVIDER, WEND_RAG_EMBEDDING_API_KEY,
#   WEND_RAG_EMBEDDING_MODEL
cargo run -- daemon
```

Migrations run automatically on startup. The MCP endpoint is available
at `http://localhost:3000/mcp`.

## Enabling Authentication

By default the HTTP transport accepts unauthenticated requests
(convenient for local development, unsafe on any reachable network).
To enable Bearer auth:

```bash
# 1. Generate a key (interactive -- prompts for a name)
cargo run -- key:generate

# 2. Start the daemon; the keys file is read automatically
cargo run -- daemon
```

Connect a client with the generated key in the `Authorization` header.
See [`resources/docs/authentication-setup.md`](resources/docs/authentication-setup.md)
for the full operator guide and
[`resources/docs/mcp-client-setup.md`](resources/docs/mcp-client-setup.md)
for per-client (Cursor, Claude, VS Code, Codex, ...) configuration.

## Further Reading

| Topic | File |
|---|---|
| Authentication (API keys) | [resources/docs/authentication-setup.md](resources/docs/authentication-setup.md) |
| MCP client configuration (Cursor, Claude, VS Code, Codex, ...) | [resources/docs/mcp-client-setup.md](resources/docs/mcp-client-setup.md) |
| Full environment variable reference | [resources/docs/configuration.md](resources/docs/configuration.md) |
| CLI flags, subcommands, and one-shot ingestion | [resources/docs/cli.md](resources/docs/cli.md) |
| MCP tool reference (incl. input size limits) | [resources/docs/mcp-tools.md](resources/docs/mcp-tools.md) |
| Retrieval modes, graph expansion, chunking | [resources/docs/retrieval.md](resources/docs/retrieval.md) |
| Entity communities and two-tier retrieval | [resources/docs/entity-communities.md](resources/docs/entity-communities.md) |
| Memory subsystem and sessions | [resources/docs/memory-sessions.md](resources/docs/memory-sessions.md) |
| End-to-end EC2 + Postgres + Ollama walkthrough | [resources/docs/setup-example.md](resources/docs/setup-example.md) |
| Source layout and key abstractions | [resources/docs/project-structure.md](resources/docs/project-structure.md) |

## License

MIT
