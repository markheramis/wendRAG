# Retrieval and Chunking

## Retrieval Modes

wendRAG supports three search modes selectable per request via the `mode` parameter on `rag_get_context`. When a returned chunk is truncated mid-block (for example, a Mermaid diagram split across chunk boundaries), use `rag_get_chunk` to pull the target chunk together with any number of contiguous neighbours.

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

When `WEND_RAG_GRAPH_RETRIEVAL_ENABLED=true`, hybrid search adds a third graph branch:

1. Dense and sparse search run as usual and are fused with RRF.
2. The top fused chunks seed entity lookup through `entity_mentions`.
3. A recursive CTE traverses `entity_relationships` up to `WEND_RAG_GRAPH_TRAVERSAL_DEPTH` hops.
4. Chunks mentioning related entities are scored as a third RRF branch and fused back into the final ranking.

The graph and community branches are executed **concurrently** via
`tokio::join!` once the initial dense + sparse fusion has produced the
seed chunks, so enabling graph retrieval does not serialise extra
round-trips against the database.

Entity extraction must be enabled at ingestion time (`WEND_RAG_ENTITY_EXTRACTION_ENABLED=true`) for graph retrieval to have data to work with. See [configuration.md](configuration.md) for endpoint and model options.

## Community-Augmented Retrieval

When graph retrieval is enabled and entities have been extracted, wendRAG also detects **entity communities** using the Louvain algorithm. Communities group related entities that frequently co-occur, enabling two-tier retrieval:

1. **Local tier** — communities containing entities from the initial search results are identified via the `community_members` table.
2. **Global tier** — communities semantically similar to the query are found via ANN search over community summary embeddings.
3. Entity IDs from matched communities are collected and used to fetch additional chunks, which are merged into the final RRF fusion as a fourth branch.

Community detection runs automatically during ingestion when a document's entity graph contains 5 or more entities. Communities are scoped per project; global communities (no project) are included in all searches.

Community summaries are generated synthetically by default (fast, deterministic). Set `WEND_RAG_COMMUNITY_LLM_SUMMARIES=true` to use LLM-generated summaries with automatic fallback to synthetic on API errors.

The `rag://communities` MCP resource lists all detected communities. See [entity-communities.md](entity-communities.md) for the full architecture guide.

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
| Any | topic boundaries | Semantic (when `WEND_RAG_CHUNKING_STRATEGY=semantic`) |

### Semantic chunking

When `WEND_RAG_CHUNKING_STRATEGY=semantic`, the chunker uses embedding
similarity to detect topic boundaries between candidate sections. The
`WEND_RAG_CHUNKING_SEMANTIC_THRESHOLD` percentile (default `0.25`)
controls sensitivity: lower values produce fewer, larger chunks; higher
values produce more, smaller chunks. `WEND_RAG_CHUNKING_MAX_SENTENCES`
(default `20`) is a hard upper bound enforced even when similarity is
high, and `WEND_RAG_CHUNKING_FILTER_GARBAGE` (default `true`) removes
navigation, ads, and boilerplate before chunking.
