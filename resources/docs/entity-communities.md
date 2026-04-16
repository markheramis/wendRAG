# Entity Communities

Entity communities group related entities detected during ingestion into clusters that enable two-tier retrieval: targeted local lookups and broad global exploration.

## How It Works

### Detection

When a document is ingested with entity extraction enabled (`WEND_RAG_ENTITY_EXTRACTION_ENABLED=true`), the pipeline:

1. Extracts entities and relationships from each chunk via an LLM.
2. Persists the entity graph (entities, mentions, relationships).
3. If the graph contains 5 or more entities, runs the **Louvain community detection algorithm** to partition entities into groups.
4. Generates a summary for each community (synthetic or LLM-based).
5. Embeds each summary for semantic search.
6. Persists communities and their entity memberships.

Communities are scoped per project. When a project is specified during ingestion, communities are tagged with that project. Communities with no project are treated as **global** and are included in all searches regardless of project filter.

### Louvain Algorithm

The implementation uses an optimized Louvain algorithm with:

- **Sparse graph representation** for memory efficiency.
- **Early termination** when modularity gain falls below a configurable threshold.
- **O(N log N) average time complexity** via batch processing.

For graphs with fewer than 5 entities, a single community is created without running the full algorithm.

### Summary Generation

Two modes are available:

| Mode | Behavior | Latency | Cost |
|------|----------|---------|------|
| **Synthetic** (default) | Concatenates entity names, types, and descriptions | <1ms | None |
| **LLM** (opt-in) | Calls an OpenAI-compatible chat endpoint | ~200ms | API tokens |

LLM mode falls back to synthetic automatically on any API error.

## Two-Tier Retrieval

Community-augmented retrieval adds a fourth branch to the existing RRF fusion pipeline (dense + sparse + graph + community):

```
Query
  |
  v
[Dense Search] ----\
[Sparse Search] ----+---> RRF Fusion (initial) ---> Seed Chunks
[Graph Search] ----/                                      |
                                                          v
                                              [Community Search]
                                                Local: communities
                                                  containing seed
                                                  entities
                                                Global: ANN search
                                                  on community
                                                  embeddings
                                                          |
                                                          v
                                              [Final RRF Fusion]
                                                (all 4 branches)
```

**Local tier**: Finds communities containing entities from the initial search results. This surfaces chunks from the same knowledge cluster.

**Global tier**: Performs ANN search over community summary embeddings to find thematically similar communities, even if they share no entities with the initial results.

## Query Routing

When no explicit search mode is specified, the **query router** classifies the query as Local, Global, or Hybrid:

- **Local queries** (e.g., "What is the API endpoint for auth?") — chunk-level retrieval, community branch may be skipped for speed.
- **Global queries** (e.g., "Give me an overview of the architecture") — community search is activated for broader context.
- **Hybrid queries** (ambiguous) — both paths run.

Classification is rule-based (O(1) keyword matching) and requires no LLM calls.

## Configuration

| Environment Variable | Default | Description |
|---------------------|---------|-------------|
| `WEND_RAG_ENTITY_EXTRACTION_ENABLED` | `false` | Master switch for entity extraction (required for communities) |
| `WEND_RAG_GRAPH_RETRIEVAL_ENABLED` | `false` | Enables graph and community retrieval branches |
| `WEND_RAG_COMMUNITY_LLM_SUMMARIES` | `false` | Use LLM-generated summaries instead of synthetic |
| `WEND_RAG_COMMUNITY_LLM_URL` | Entity extraction URL | LLM endpoint for community summaries |
| `WEND_RAG_COMMUNITY_LLM_MODEL` | Entity extraction model | Model for community summaries |
| `WEND_RAG_COMMUNITY_LLM_API_KEY` | Entity extraction key | API key for community summaries |

### YAML Configuration

```yaml
entity_extraction:
  enabled: true
  base_url: "http://localhost:11434"
  model: "llama3.2"

graph:
  enabled: true
  traversal_depth: 2

community:
  llm_summaries: false
  base_url: "http://localhost:11434"
  model: "llama3.2"
```

## MCP Resources

### `rag://communities`

Lists all detected communities with their metadata:

```json
{
  "total": 5,
  "communities": [
    {
      "id": "a1b2c3d4-...",
      "name": "Authentication + Authorization and 3 others",
      "summary": "Community: Authentication + Authorization and 3 others. Contains 5 entities of types: SERVICE, CONCEPT. Key members: ...",
      "project": "my-project",
      "importance": 2.45,
      "entity_count": 5
    }
  ]
}
```

## Database Schema

### PostgreSQL

Communities use pgvector for HNSW-indexed embedding search:

```sql
entity_communities (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    summary TEXT,
    project TEXT,          -- NULL = global scope
    importance FLOAT,
    embedding vector(1024),
    created_at TIMESTAMPTZ
)

community_members (
    community_id UUID REFERENCES entity_communities(id),
    entity_id UUID REFERENCES entities(id),
    PRIMARY KEY (community_id, entity_id)
)
```

### SQLite

Same schema with TEXT IDs and BLOB embeddings. Cosine similarity is computed in Rust for ANN search.

## Performance

| Operation | Expected Latency |
|-----------|-----------------|
| Community detection (100 entities) | 5-20ms |
| Synthetic summary generation | <1ms per community |
| Summary embedding (batch of 10) | ~100ms (API bound) |
| Community ANN search (PostgreSQL) | 1-5ms |
| Community cosine scan (SQLite, 100 rows) | <0.5ms |
| Query classification | <0.1ms |
