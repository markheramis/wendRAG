# Retrieval and Chunking

## Retrieval Modes

wendRAG supports three search modes selectable per request via the `mode` parameter on `rag_get_context` and `rag_get_full_context`.

### Dense retrieval

Uses embedding similarity to find semantically related chunks.

- **PostgreSQL** — `pgvector` cosine similarity.
- **SQLite** — embedding blobs loaded into memory; cosine similarity computed in pure Rust.

### Sparse retrieval

Uses keyword and structural matching.

- **PostgreSQL** — full-text search plus trigram (`pg_trgm`) matching.
- **SQLite** — FTS5 term search plus trigram-backed title and path matching.

### Hybrid retrieval (default)

Runs dense and sparse branches in parallel, then combines the ranked results using **Reciprocal Rank Fusion (RRF)**. RRF scores by position rather than raw score, making the two retrieval paths comparable without score normalization.

## Graph-Augmented Retrieval

When `GRAPH_RETRIEVAL_ENABLED=true`, hybrid search adds a third graph branch:

1. Dense and sparse search run as usual and are fused with RRF.
2. The top fused chunks seed entity lookup through `entity_mentions`.
3. A recursive CTE traverses `entity_relationships` up to `GRAPH_TRAVERSAL_DEPTH` hops.
4. Chunks mentioning related entities are scored as a third RRF branch and fused back into the final ranking.

Entity extraction must be enabled at ingestion time (`ENTITY_EXTRACTION_ENABLED=true`) for graph retrieval to have data to work with. See [configuration.md](configuration.md) for endpoint and model options.

---

## Chunking

Chunking is a two-step process: structural splitting always runs first, then oversized sections are split again according to the configured strategy.

### Structural splitting

| Document type | Split strategy |
|---|---|
| Markdown, URL | H1 / H2 / H3 heading boundaries |
| Text, PDF | Paragraph merging up to 1 000 characters |

### Oversized section splitting

After structural splitting, any section that still exceeds the per-type limit is split again:

| Document type | Limit | Mode |
|---|---|---|
| Markdown, URL | 4 000 characters | Fixed window |
| Text, PDF | 1 000 characters | Fixed window with 200-character overlap |
| Any | topic boundaries | Semantic (when `CHUNKING_STRATEGY=semantic`) |

### Semantic chunking

When `CHUNKING_STRATEGY=semantic`, the chunker uses embedding similarity to detect topic boundaries between candidate sections. The `CHUNKING_SEMANTIC_THRESHOLD` percentile (default `0.25`) controls sensitivity: lower values produce fewer, larger chunks; higher values produce more, smaller chunks.
