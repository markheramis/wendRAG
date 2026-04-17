# MCP Client Setup

This document explains how to register wendRAG as an MCP server across every
major MCP client environment. It covers both transports:

- **Streamable HTTP** (recommended for most deployments) -- wendRAG runs as a
  long-lived daemon on `http://<host>:<port>/mcp`.
- **stdio** -- the client spawns `wend-rag stdio` as a child process and
  communicates over stdin/stdout.

See [`authentication-setup.md`](authentication-setup.md) for how to mint
API keys that secure the HTTP transport.

## Supported transports at a glance

| Transport | Start command      | Endpoint                  | Auth |
|-----------|--------------------|---------------------------|------|
| HTTP      | `wend-rag daemon`  | `http://<host>:<port>/mcp`| Optional Bearer token |
| stdio     | `wend-rag stdio`   | stdin/stdout              | Inherited from parent |

## Environment-specific configuration

Each MCP client has its own configuration file, root key, and format. The
tables below use this canonical wendRAG URL, which you can replace with your
own host/port and API key:

```
URL:        http://localhost:3000/mcp
API key:    wrag_your_generated_key_here
```

---

### Cursor

**File location:**

- Global: `~/.cursor/mcp.json`
- Project: `<repo>/.cursor/mcp.json`

Cursor hot-reloads the config -- no restart needed after edits.

**HTTP transport with API key:**

```json
{
  "mcpServers": {
    "wendRAG": {
      "type": "http",
      "url": "http://localhost:3000/mcp",
      "headers": {
        "Authorization": "Bearer wrag_your_generated_key_here"
      }
    }
  }
}
```

**HTTP transport using the `api_key` shorthand** (as in the user-provided
example):

```json
{
  "mcpServers": {
    "wendRAG": {
      "type": "url",
      "url": "http://localhost:3000/mcp",
      "api_key": "wrag_your_generated_key_here"
    }
  }
}
```

**stdio transport:**

```json
{
  "mcpServers": {
    "wendRAG": {
      "type": "stdio",
      "command": "wend-rag",
      "args": ["stdio"],
      "env": {
        "WEND_RAG_DATABASE_URL": "postgres://user:pass@localhost/wendrag",
        "WEND_RAG_EMBEDDING_API_KEY": "sk-..."
      }
    }
  }
}
```

---

### Claude Desktop

**File location:**

| OS      | Path |
|---------|------|
| macOS   | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Windows | `%APPDATA%\Claude\claude_desktop_config.json` |
| Linux   | `~/.config/Claude/claude_desktop_config.json` |

Claude Desktop requires a full application restart after edits.

**stdio transport (officially supported):**

```json
{
  "mcpServers": {
    "wendRAG": {
      "command": "wend-rag",
      "args": ["stdio"],
      "env": {
        "WEND_RAG_DATABASE_URL": "postgres://user:pass@localhost/wendrag",
        "WEND_RAG_EMBEDDING_API_KEY": "sk-..."
      }
    }
  }
}
```

**HTTP transport:** Native HTTP support in Claude Desktop is still evolving.
If your installed version supports remote servers, the config follows the
same `url` + `headers` pattern as Cursor. For older versions, run a local
stdio-to-HTTP bridge (e.g. `mcp-proxy`) and point Claude at the bridge over
stdio.

---

### VS Code / GitHub Copilot

**File location:**

- User: `~/.vscode/mcp.json` (or the `.mcp.json` in your VS Code profile
  directory)
- Workspace: `<repo>/.vscode/mcp.json`

**Root key:** `servers` (not `mcpServers`).

**HTTP transport with API key:**

```json
{
  "servers": {
    "wendRAG": {
      "type": "http",
      "url": "http://localhost:3000/mcp",
      "headers": {
        "Authorization": "Bearer wrag_your_generated_key_here"
      }
    }
  },
  "inputs": []
}
```

**stdio transport:**

```json
{
  "servers": {
    "wendRAG": {
      "type": "stdio",
      "command": "wend-rag",
      "args": ["stdio"],
      "envFile": "${workspaceFolder}/.env"
    }
  }
}
```

The `inputs` array can prompt the user for secrets at first load. See the
[official docs](https://code.visualstudio.com/docs/copilot/mcp) for the
`${input:<id>}` syntax.

---

### OpenAI Codex CLI

**File location:** `~/.codex/config.toml` (TOML, not JSON).

**Root key:** `mcp_servers`.

**HTTP transport:**

```toml
[mcp_servers.wendRAG]
type = "http"
url = "http://localhost:3000/mcp"

[mcp_servers.wendRAG.headers]
Authorization = "Bearer wrag_your_generated_key_here"
```

**stdio transport:**

```toml
[mcp_servers.wendRAG]
command = "wend-rag"
args = ["stdio"]

[mcp_servers.wendRAG.env]
WEND_RAG_DATABASE_URL = "postgres://user:pass@localhost/wendrag"
WEND_RAG_EMBEDDING_API_KEY = "sk-..."
```

---

### Windsurf

**File location:** `~/.codeium/windsurf/mcp_config.json`.

**Root key:** `mcpServers` (same shape as Cursor/Claude).

```json
{
  "mcpServers": {
    "wendRAG": {
      "command": "wend-rag",
      "args": ["stdio"]
    }
  }
}
```

HTTP transport shape (where supported) matches Cursor's `type: "http"` form.

---

### Zed

**File location:** `~/.config/zed/settings.json`.

**Root key:** `context_servers`.

```json
{
  "context_servers": {
    "wendRAG": {
      "command": {
        "path": "wend-rag",
        "args": ["stdio"]
      }
    }
  }
}
```

---

### Generic clients / integration tests (raw HTTP)

Any HTTP client that speaks JSON-RPC 2.0 over POST can call wendRAG directly.

**Health check (no auth ever required):**

```bash
curl http://localhost:3000/health
# => {"status":"ok"}
```

**MCP initialize handshake (with auth):**

```bash
curl -X POST http://localhost:3000/mcp \
  -H "Authorization: Bearer wrag_your_generated_key_here" \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc":"2.0",
    "id":1,
    "method":"initialize",
    "params":{
      "protocolVersion":"2024-11-05",
      "capabilities":{},
      "clientInfo":{"name":"curl","version":"1.0"}
    }
  }'
```

**Listing tools:**

```bash
curl -X POST http://localhost:3000/mcp \
  -H "Authorization: Bearer wrag_your_generated_key_here" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
```

---

## Configuration reference

### `type` field values by client

| Client         | HTTP value        | stdio value |
|----------------|-------------------|-------------|
| Cursor         | `"http"` / `"url"`| `"stdio"` (implicit when `command` is set) |
| VS Code        | `"http"`          | `"stdio"` |
| Claude Desktop | `"http"` (newer)  | *(omit; implicit when `command` is set)* |
| Codex          | `"http"`          | *(implicit)* |
| Windsurf       | *(implicit)*      | *(implicit)* |
| Zed            | *(stdio-only)*    | via `command.path` |

### Common fields

| Field          | Type    | Purpose |
|----------------|---------|---------|
| `url`          | string  | HTTP endpoint, always `<base>/mcp` for wendRAG |
| `type`         | string  | Transport selector (varies by client) |
| `headers`      | object  | HTTP headers; use for `Authorization` |
| `api_key`      | string  | Shorthand alternative to `Authorization` (Cursor) |
| `command`      | string  | Executable for stdio transport |
| `args`         | array   | CLI arguments passed to `command` |
| `env`          | object  | Environment variables for the stdio child process |
| `envFile`      | string  | Path to a `.env` file (VS Code) |

### Environment variables wendRAG honours

Passed through `env` (stdio) or set on the host before launching `daemon`:

| Variable                              | Purpose |
|---------------------------------------|---------|
| `WEND_RAG_HOST` / `WEND_RAG_PORT`     | Bind address for `daemon` |
| `WEND_RAG_DATABASE_URL`               | PostgreSQL connection string |
| `WEND_RAG_STORAGE_BACKEND`            | `postgres` or `sqlite` |
| `WEND_RAG_SQLITE_PATH`                | SQLite database path |
| `WEND_RAG_EMBEDDING_API_KEY`          | Embedding provider credential |
| `WEND_RAG_EMBEDDING_BASE_URL`         | Embedding provider endpoint |
| `WEND_RAG_EMBEDDING_MODEL`            | Embedding model name |
| `WEND_RAG_EMBEDDING_PROVIDER`         | `openai` / `voyage` / `ollama` / `openai-compatible` |
| `WEND_RAG_ENTITY_EXTRACTION_ENABLED`  | `true` / `false` |
| `WEND_RAG_GRAPH_RETRIEVAL_ENABLED`    | `true` / `false` |
| `WEND_RAG_MEMORY_ENABLED`             | `true` / `false` |
| `WEND_RAG_API_KEY`                    | Single static Bearer token (HTTP auth) |
| `WEND_RAG_KEYS_FILE`                  | Override for the keys JSON file path |
| `WEND_RAG_CONFIG`                     | Override for the YAML config path |

The canonical reference for every variable and its default lives in
[`configuration.md`](configuration.md).

---

## Troubleshooting

### The client reports "server not found" or similar

- Confirm the daemon is listening: `curl http://localhost:3000/health`.
- Double-check `url` points to `/mcp`, not `/` or `/api`.
- Make sure the client reloaded the config (Cursor reloads automatically;
  Claude Desktop and VS Code usually need a restart).

### Requests work with curl but fail in the client

- Some clients expect the MCP session header (`Mcp-Session-Id`) to be
  echoed back -- this is handled by wendRAG automatically.
- Make sure the client is sending a POST, not a GET, to `/mcp`.

### 401 Unauthorized

- See the troubleshooting section of
  [`authentication-setup.md`](authentication-setup.md).

### `spawn wend-rag ENOENT` on stdio

- Ensure `wend-rag` is on the system `PATH` as visible to the client. GUI
  apps on macOS often don't inherit shell PATH; use the absolute path
  (`/usr/local/bin/wend-rag` or similar) as the `command` value.

### Stdio server seems to hang

- wendRAG logs to stderr. Launch it by hand (`wend-rag stdio < /dev/null`)
  to see startup errors that the client may be swallowing.
