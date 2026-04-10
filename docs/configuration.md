# Configuration

All runtime settings come from environment variables or a `.env` file loaded at startup. Copy `.env.example` to `.env` and edit the values to match your environment.

## Core Settings

| Variable | Default | Notes |
|---|---|---|
| `HOST` | `0.0.0.0` | Bind address for the HTTP server. |
| `PORT` | `3000` | HTTP port. |
| `MCP_TRANSPORT` | `http` | `http` (Streamable HTTP on `/mcp`) or `stdio`. The `--stdio` CLI flag takes precedence when present. |
| `STORAGE_BACKEND` | auto-detected | `postgres` or `sqlite`. If unset, uses `postgres` when `DATABASE_URL` is set; otherwise falls back to `sqlite`. |
| `DATABASE_URL` | — | PostgreSQL connection string. Required when `STORAGE_BACKEND=postgres`. |
| `SQLITE_PATH` | `./wend-rag.db` | SQLite database file path. Used when `STORAGE_BACKEND=sqlite`. |

## Embedding

Both backends expect **1024-dimensional** embeddings. Choose a provider and model that already emits 1024-dimensional vectors.

| Variable | Default | Notes |
|---|---|---|
| `EMBEDDING_PROVIDER` | `openai` | `openai`, `voyage`, or `openai-compatible`. |
| `EMBEDDING_API_KEY` | required | API key for the embedding service. Set to any non-empty string for local providers (e.g. `ollama`). |
| `EMBEDDING_BASE_URL` | provider-specific | Override the embeddings base URL. Required for `openai-compatible` (e.g. `http://localhost:11434/v1/`). |
| `EMBEDDING_MODEL` | provider-specific | Embedding model name. The runtime does not request custom dimensions. |
| `EMBEDDING_DIMENSIONS` | `1024` | Parsed from environment; used for schema alignment. |

### Examples

**Ollama (local, no API key):**

```env
EMBEDDING_PROVIDER=openai-compatible
EMBEDDING_API_KEY=ollama
EMBEDDING_MODEL=bge-m3:latest
EMBEDDING_BASE_URL=http://localhost:11434/v1/
EMBEDDING_DIMENSIONS=1024
```

**OpenAI:**

```env
EMBEDDING_PROVIDER=openai
EMBEDDING_API_KEY=sk-your-key-here
EMBEDDING_MODEL=text-embedding-3-large
EMBEDDING_DIMENSIONS=1024
```

**Voyage AI:**

```env
EMBEDDING_PROVIDER=voyage
EMBEDDING_API_KEY=pa-your-key-here
EMBEDDING_MODEL=voyage-3-large
EMBEDDING_BASE_URL=https://api.voyageai.com
EMBEDDING_DIMENSIONS=1024
```

## Entity Extraction and Graph Retrieval

Entity extraction and graph retrieval are both disabled by default and work on both PostgreSQL and SQLite backends.

| Variable | Default | Notes |
|---|---|---|
| `ENTITY_EXTRACTION_ENABLED` | `false` | Enables ingestion-time entity extraction. Persists entity mentions and relationships. |
| `ENTITY_EXTRACTION_LLM_URL` | falls back to `EMBEDDING_BASE_URL` | OpenAI-compatible chat completions endpoint for extraction. |
| `ENTITY_EXTRACTION_LLM_MODEL` | `gpt-4.1-mini` | Model name. Examples: `llama3.2` (Ollama), `gpt-4.1-mini` (OpenAI), `claude-haiku-4-5-20251001` (Anthropic). |
| `ENTITY_EXTRACTION_API_KEY` | falls back to `EMBEDDING_API_KEY` | API key for the extraction endpoint. Not required for local Ollama. |
| `GRAPH_RETRIEVAL_ENABLED` | `false` | Adds a graph branch to `mode="hybrid"` searches seeded from dense+sparse results. |
| `GRAPH_TRAVERSAL_DEPTH` | `2` | Recursive traversal depth for entity expansion. Clamped to `1..=3`. |

### Extraction endpoint examples

```env
# Ollama (local, no API key required)
ENTITY_EXTRACTION_LLM_URL=http://localhost:11434
ENTITY_EXTRACTION_LLM_MODEL=llama3.2

# OpenAI
ENTITY_EXTRACTION_LLM_URL=https://api.openai.com
ENTITY_EXTRACTION_LLM_MODEL=gpt-4.1-mini
ENTITY_EXTRACTION_API_KEY=sk-your-key-here

# Anthropic (OpenAI-compatible endpoint)
ENTITY_EXTRACTION_LLM_URL=https://api.anthropic.com/v1
ENTITY_EXTRACTION_LLM_MODEL=claude-haiku-4-5-20251001
ENTITY_EXTRACTION_API_KEY=sk-ant-your-key-here
```

## Chunking

| Variable | Default | Notes |
|---|---|---|
| `CHUNKING_STRATEGY` | `fixed` | `fixed` (character-window overlap) or `semantic` (embedding-similarity boundaries). |
| `CHUNKING_SEMANTIC_THRESHOLD` | `0.25` | Percentile for semantic breakpoints (0.0–1.0). Lower = fewer, larger chunks. |

## Docker Compose Notes

Docker Compose loads `.env` and then overrides `STORAGE_BACKEND`, `DATABASE_URL`, `HOST`, and `PORT` via the `environment:` block in `compose.yml` so the server always talks to the compose-managed PostgreSQL service rather than localhost. The Postgres service is published on host port **15432** to avoid Windows Hyper-V/WSL excluded port ranges.
