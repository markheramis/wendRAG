# Memory and Sessions

The memory layer provides persistent, agent-oriented memory with session continuity and long-term knowledge accumulation. It enables AI agents to remember context across conversations, store facts and preferences, and retrieve relevant memories using semantic search.

## Architecture

The memory system has three tiers:

- **Session memory** (in-memory): Short-term conversation buffer per session. Messages are stored in a sliding window with optional summarization. Session buffers live in a `DashMap` and expire after a configurable timeout.
- **User memory** (persistent): Long-term facts, preferences, and decisions tied to a specific user. Stored in the database with embedding vectors for semantic retrieval.
- **Global memory** (persistent): Shared knowledge accessible to all users and sessions.

## Memory Protocol

When the memory subsystem is enabled, agents should follow this behavioral contract (also available via the `rag://memory/status` resource):

1. **Search before answering** questions about past interactions or user preferences.
2. **Store important information** — facts, preferences, and decisions worth remembering.
3. **Invalidate stale facts** when corrections are provided rather than creating duplicates.

## MCP Tools

### `memory_store`

Store a memory entry for later retrieval.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `content` | string | required | The content to remember (≤ 1 MiB) |
| `scope` | string | `"user"` | `"session"`, `"user"`, or `"global"` |
| `entry_type` | string | `"fact"` | `"fact"`, `"preference"`, `"event"`, `"summary"`, or `"message"` |
| `user_id` | string | - | User identifier for user-scoped memories |
| `session_id` | string | - | Session identifier for session-scoped memories |
| `importance` | float | `0.5` | Importance score (0.0 to 1.0) |

Content exceeding 1 MiB is rejected before any embedding call runs. See
[mcp-tools.md](mcp-tools.md#input-size-limits) for the full set of
server-side caps.

### `memory_retrieve`

Search stored memories using semantic similarity.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `query` | string | required | Natural language search query (≤ 10 KiB) |
| `user_id` | string | - | Filter by user ID |
| `session_id` | string | - | Filter by session ID |
| `scope` | string | - | Filter by scope |
| `limit` | int | `10` | Maximum results |

### `memory_forget`

Delete or invalidate a memory entry.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `memory_id` | string | required | UUID of the memory to forget |
| `invalidate` | bool | `true` | `true` = soft-delete (mark invalid), `false` = hard delete |

### `memory_sessions`

Manage active sessions.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `action` | string | `"list"` | `"list"`, `"get"`, or `"end"` |
| `session_id` | string | - | Required for `"get"` and `"end"` actions |
| `user_id` | string | - | User ID to associate when ending a session |

## Memory Lifecycle

1. **Store**: Content is embedded and persisted with scope, type, and importance.
2. **Reinforce**: Each retrieval updates `last_accessed` and `access_count`, boosting the memory's ranking in future queries.
3. **Decay**: The maintenance task applies Ebbinghaus-style exponential decay (`importance *= e^(-alpha * days)`), gradually lowering importance of unaccessed memories.
4. **Prune**: Memories below the `prune_threshold` are deleted during maintenance.
5. **Invalidate**: Stale facts can be soft-deleted via `memory_forget` with `invalidate=true`. Invalidated entries are excluded from queries and hard-deleted after 7 days.

## MCP Resource

### `rag://memory/status`

Returns memory subsystem status including active session count, memory totals by scope, and the behavioral protocol:

```json
{
  "enabled": true,
  "active_sessions": 3,
  "total_memories": 847,
  "memories_by_scope": { "session": 120, "user": 680, "global": 47 },
  "protocol": "Search memory before answering questions about past interactions. Store important facts, preferences, and decisions. Invalidate stale facts when corrections are provided."
}
```

## Configuration

| Environment Variable | Default | Description |
|---------------------|---------|-------------|
| `WEND_RAG_MEMORY_ENABLED` | `false` | Enable the memory subsystem |
| `WEND_RAG_MEMORY_SESSION_TIMEOUT` | `3600` | Seconds before a session expires |
| `WEND_RAG_MEMORY_DECAY_RATE` | `0.02` | Ebbinghaus alpha parameter for importance decay |
| `WEND_RAG_MEMORY_PRUNE_THRESHOLD` | `0.3` | Minimum importance score before pruning |
| `WEND_RAG_MEMORY_MAX_PER_QUERY` | `20` | Maximum memories returned per query |
| `WEND_RAG_MEMORY_RECENCY_WEIGHT` | `0.3` | Balance between relevance (0.0) and recency (1.0) |

### YAML Configuration

```yaml
memory:
  enabled: true
  session_timeout: 3600
  decay_rate: 0.02
  prune_threshold: 0.3
  max_per_query: 20
  recency_weight: 0.3
```

## Storage

Memory entries are stored in `memory_entries` with optional embedding vectors for semantic search. Both PostgreSQL (pgvector HNSW) and SQLite (BLOB with in-Rust cosine) backends are supported.

Entity links (`memory_entity_links`) connect memories to the entity graph, enabling graph-augmented memory recall.

## Design Decisions

- **Verbatim storage by default**: Stores content as-is for maximum retrieval recall (informed by MemPalace's 96.6% R@5 on raw storage).
- **Decay for ranking, invalidation for correctness**: Decay naturally deprioritizes old memories; explicit invalidation handles factual corrections (informed by Zep's temporal truth model).
- **Non-generic MemoryManager**: Accepts `Arc<dyn MemoryStorage>` to avoid generic parameter propagation through the server layer.
- **Background maintenance**: A tokio interval task handles decay, pruning, and session cleanup without blocking request processing.
