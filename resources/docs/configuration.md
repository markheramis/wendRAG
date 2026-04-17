# Configuration

wendRAG settings come from three sources, merged in order of increasing priority:

1. **Compiled defaults** (lowest)
2. **`WEND_RAG_*` environment variables** (including anything loaded from a local `.env` file via `dotenvy`)
3. **YAML config file** (highest; see `config.example.yaml` at the repo root)

The YAML file is resolved from, in order:

- the `-c/--config <path>` CLI flag,
- the `WEND_RAG_CONFIG` environment variable,
- `/etc/wend-rag/config.yaml` on Linux (if it exists).

Every environment variable uses the `WEND_RAG_` prefix. The examples in this document keep that prefix; the YAML config file uses nested keys instead (see `config.example.yaml`).

## Server

| Variable | Default | Notes |
|---|---|---|
| `WEND_RAG_HOST` | `0.0.0.0` | Bind address for the HTTP server. Set to `127.0.0.1` if you only want local clients. |
| `WEND_RAG_PORT` | `3000` | HTTP port. |
| `WEND_RAG_CONFIG` | *(unset)* | Overrides the YAML config path. |

Transport selection is controlled by the CLI subcommand, not by an env var:
`wend-rag daemon` starts Streamable HTTP on `/mcp`; `wend-rag stdio` speaks MCP over stdin/stdout.

## Authentication (HTTP transport)

Authentication is **optional**. When neither variable is set and the keys file is empty, `/mcp` accepts unauthenticated requests (useful for local development, unsafe on any reachable network). See [authentication-setup.md](authentication-setup.md) for the full operator guide.

| Variable | Default | Notes |
|---|---|---|
| `WEND_RAG_API_KEY` | *(unset)* | Single static Bearer token. When set, every `/mcp` request must present it in `Authorization: Bearer <token>`. |
| `WEND_RAG_KEYS_FILE` | platform default | Override for the `keys.json` path used by `wend-rag key:generate / key:list / key:revoke`. Default is `$HOME/.wend-rag/keys.json` on Unix/macOS and `%APPDATA%\wend-rag\keys.json` on Windows. |

`/health` is always unauthenticated so systemd and load-balancer probes work without a token.

## Storage

| Variable | Default | Notes |
|---|---|---|
| `WEND_RAG_STORAGE_BACKEND` | auto-detected | `postgres` or `sqlite`. If unset, `postgres` is used when `WEND_RAG_DATABASE_URL` is present; otherwise `sqlite`. |
| `WEND_RAG_DATABASE_URL` | *(unset)* | PostgreSQL connection string. Required when `WEND_RAG_STORAGE_BACKEND=postgres`. |
| `WEND_RAG_SQLITE_PATH` | `./wend-rag.db` | SQLite database file path. Use `:memory:` for tests. |
| `WEND_RAG_POOL_MAX_CONNECTIONS` | `20` | Shared pool size for both backends. |
| `WEND_RAG_POOL_ACQUIRE_TIMEOUT_SECS` | `60` | Pool acquire timeout, in seconds. |

## Embedding

Both backends expect **1024-dimensional** embeddings. Choose a provider and model that already emits 1024-dimensional vectors.

| Variable | Default | Notes |
|---|---|---|
| `WEND_RAG_EMBEDDING_PROVIDER` | `openai` | `openai`, `voyage`, `ollama`, or `openai-compatible`. |
| `WEND_RAG_EMBEDDING_API_KEY` | *(empty)* | API key for the embedding service. Set to any non-empty placeholder for local providers. |
| `WEND_RAG_EMBEDDING_BASE_URL` | provider-specific | Override the embeddings base URL. Required for `openai-compatible`. |
| `WEND_RAG_EMBEDDING_MODEL` | provider-specific | Embedding model name. The runtime does not request custom dimensions. |
| `WEND_RAG_EMBEDDING_DIMENSIONS` | provider-specific | Vector dimensions for schema alignment (1024 for most configs). |

### Examples

**Ollama (local, no API key):**

```env
WEND_RAG_EMBEDDING_PROVIDER=openai-compatible
WEND_RAG_EMBEDDING_API_KEY=ollama
WEND_RAG_EMBEDDING_MODEL=bge-m3:latest
WEND_RAG_EMBEDDING_BASE_URL=http://localhost:11434/v1/
WEND_RAG_EMBEDDING_DIMENSIONS=1024
```

**OpenAI:**

```env
WEND_RAG_EMBEDDING_PROVIDER=openai
WEND_RAG_EMBEDDING_API_KEY=sk-your-key-here
WEND_RAG_EMBEDDING_MODEL=text-embedding-3-large
WEND_RAG_EMBEDDING_DIMENSIONS=1024
```

**Voyage AI:**

```env
WEND_RAG_EMBEDDING_PROVIDER=voyage
WEND_RAG_EMBEDDING_API_KEY=pa-your-key-here
WEND_RAG_EMBEDDING_MODEL=voyage-3-large
WEND_RAG_EMBEDDING_BASE_URL=https://api.voyageai.com
WEND_RAG_EMBEDDING_DIMENSIONS=1024
```

## Entity Extraction and Graph Retrieval

Entity extraction and graph retrieval are both disabled by default and work on both backends.

| Variable | Default | Notes |
|---|---|---|
| `WEND_RAG_ENTITY_EXTRACTION_ENABLED` | `false` | Enables ingestion-time entity extraction. |
| `WEND_RAG_ENTITY_EXTRACTION_LLM_URL` | falls back to `WEND_RAG_EMBEDDING_BASE_URL` | OpenAI-compatible chat completions endpoint. |
| `WEND_RAG_ENTITY_EXTRACTION_LLM_MODEL` | `gpt-4.1-mini` | Model name. Examples: `llama3.2` (Ollama), `gpt-4.1-mini` (OpenAI). |
| `WEND_RAG_ENTITY_EXTRACTION_API_KEY` | falls back to `WEND_RAG_EMBEDDING_API_KEY` | API key for the extraction endpoint. |
| `WEND_RAG_GRAPH_RETRIEVAL_ENABLED` | `false` | Adds a graph branch to `mode="hybrid"` searches seeded from dense + sparse results. |
| `WEND_RAG_GRAPH_TRAVERSAL_DEPTH` | `2` | Recursive traversal depth for entity expansion. Clamped to `1..=3`. |

See [entity-communities.md](entity-communities.md) for the community-augmented retrieval layer that sits on top of these.

## Communities (LLM summaries)

Community detection runs automatically when entity extraction is enabled and a document has ≥ 5 entities. Summaries default to synthetic (fast, deterministic); opt into LLM summaries with:

| Variable | Default | Notes |
|---|---|---|
| `WEND_RAG_COMMUNITY_LLM_SUMMARIES` | `false` | Use LLM-generated summaries with automatic fallback to synthetic on error. |
| `WEND_RAG_COMMUNITY_LLM_URL` | falls back to entity-extraction URL | Chat completions endpoint. |
| `WEND_RAG_COMMUNITY_LLM_MODEL` | falls back to entity-extraction model | Model name. |
| `WEND_RAG_COMMUNITY_LLM_API_KEY` | falls back to entity-extraction key | API key. |

## Reranker

A post-retrieval reranking stage is available for all search modes.

| Variable | Default | Notes |
|---|---|---|
| `WEND_RAG_RERANKER_ENABLED` | `false` | Enables reranking of fused results. |
| `WEND_RAG_RERANKER_PROVIDER` | `openai-compatible` | `cohere`, `jina`, or `openai-compatible`. |
| `WEND_RAG_RERANKER_BASE_URL` | *(empty)* | Provider endpoint. |
| `WEND_RAG_RERANKER_MODEL` | `rerank-v3.5` | Model name. |
| `WEND_RAG_RERANKER_API_KEY` | falls back to `WEND_RAG_EMBEDDING_API_KEY` | API key. |
| `WEND_RAG_RERANKER_TOP_N` | provider default | Number of candidates fetched before reranking. |

## Chunking

| Variable | Default | Notes |
|---|---|---|
| `WEND_RAG_CHUNKING_STRATEGY` | `fixed` | `fixed` (character-window overlap) or `semantic` (embedding-similarity boundaries). |
| `WEND_RAG_CHUNKING_SEMANTIC_THRESHOLD` | `0.25` | Percentile (0.0–1.0) for semantic breakpoints. Lower = fewer, larger chunks. |
| `WEND_RAG_CHUNKING_MAX_SENTENCES` | `20` | Maximum sentences per chunk; a hard upper bound even when similarity is high. |
| `WEND_RAG_CHUNKING_FILTER_GARBAGE` | `true` | Pre-filter navigation, ads, copyright notices, and very short / long sentences before chunking. |

## Memory Subsystem

The memory subsystem provides session continuity and long-term knowledge accumulation. Disabled by default. See [memory-sessions.md](memory-sessions.md) for the full guide.

| Variable | Default | Notes |
|---|---|---|
| `WEND_RAG_MEMORY_ENABLED` | `false` | Master switch for the memory layer. |
| `WEND_RAG_MEMORY_SESSION_TIMEOUT` | `3600` | Seconds of inactivity before a session buffer expires. |
| `WEND_RAG_MEMORY_DECAY_RATE` | `0.02` | Ebbinghaus `α` for per-day importance decay. |
| `WEND_RAG_MEMORY_PRUNE_THRESHOLD` | `0.3` | Minimum importance retained by the maintenance task. |
| `WEND_RAG_MEMORY_MAX_PER_QUERY` | `20` | Maximum memories returned per `memory_retrieve` call. |
| `WEND_RAG_MEMORY_RECENCY_WEIGHT` | `0.3` | Balance between relevance (0.0) and recency (1.0) in the retrieval score. |

## Observability

wendRAG emits JSON-formatted structured logs to stderr. OpenTelemetry export is optional.

| Variable | Default | Notes |
|---|---|---|
| `RUST_LOG` | `info` | Log level filter. Accepts the standard `env_logger` / `tracing-subscriber` syntax. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | *(unset)* | OTLP/gRPC endpoint. When set, traces export to this collector. |
| `OTEL_SERVICE_NAME` | `wend-rag` | Service name used in exported spans. |

## Input size limits (server-side)

MCP tool handlers enforce strict size caps to prevent memory-exhaustion and expensive-embedding attacks. These are compile-time constants; there are no matching env vars by design.

| Limit | Value | Applies to |
|---|---|---|
| Max content size | 1 MiB | `rag_ingest.content`, `memory_store.content`, each `rag_ingest_batch.documents[].content` |
| Max query size | 10 KiB | `rag_get_context.query`, `memory_retrieve.query` |
| Max batch items | 100 | `rag_ingest_batch.documents` |

Requests exceeding any cap are rejected with a descriptive JSON error before any database or embedding work runs.

## Docker Compose Notes

`compose.yml` loads `.env` and then overrides `WEND_RAG_STORAGE_BACKEND`, `WEND_RAG_DATABASE_URL`, `WEND_RAG_HOST`, and `WEND_RAG_PORT` in the `environment:` block so the server always talks to the compose-managed PostgreSQL service rather than localhost. The Postgres service is published on host port **15432** to avoid Windows Hyper-V / WSL excluded port ranges.
