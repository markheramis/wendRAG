# Setup Example (wendRAG with PostgreSQL + Ollama on Ubuntu)

## Target Environment

This tutorial sets up wendRAG on an **AWS EC2 `c6i.xlarge`** instance running **Ubuntu 24.04 LTS**.

**Instance Type**: c6i.xlarge (4 vCPU, 8 GB RAM)
**Operating System**: Ubuntu 24.04 LTS
**Storage backend**: PostgreSQL 16 + pgvector (local or AWS RDS)
**Embedding model**: bge-m3 via Ollama (local, no API key required)

When launching your EC2 instance, ensure your security group allows:

- **Port 22** — SSH access from your IP
- **Port 3000** — wendRAG MCP server (from your application or MCP client)
- **Port 5432** — PostgreSQL (only if using local PostgreSQL and connecting remotely; skip if RDS handles this separately)

SSH into your instance before starting:

```bash
ssh -i your-key.pem ubuntu@your-ec2-public-ip
```

---

## Step 1 — Install Prerequisites

### Rust (1.85+)

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
rustc --version
```
### Git

```bash
sudo apt update && sudo apt install -y git
```

---

## Step 2 — Install Ollama and Pull the Embedding Model

```bash
curl -fsSL https://ollama.com/install.sh | sh
```

Pull the bge-m3 embedding model (1024-dimensional, no API key required):

```bash
ollama pull bge-m3
```

Verify it's available:

```bash
ollama list
# bge-m3:latest should appear
```

---

## Step 3 — Clone wendRAG

```bash
git clone https://github.com/wend-ai/wendRAG.git
cd wendRAG
```

---

## Step 4 — Set Up PostgreSQL

Choose one path depending on your setup.

---

### Option A — Local PostgreSQL (on the EC2 instance)

```bash
sudo apt install -y postgresql postgresql-contrib postgresql-16-pgvector
sudo systemctl status postgresql   # confirm it's running
```

Pre-install the `vector` extension in `template1` so every database created later inherits it automatically — required for migrations to run as a non-superuser:

```bash
sudo -u postgres psql -d template1 -c "CREATE EXTENSION IF NOT EXISTS vector;"
```

Then proceed to **Step 5** and use `localhost:5432` as your host.

---

### Option B — AWS RDS

**Skip installation entirely.** Instead:

1. Create an RDS instance in the AWS console with:
    
    - Engine: **PostgreSQL 16** (pgvector is built in on RDS PostgreSQL 14.5+)
    - Place it in the **same VPC as your EC2 instance** for private connectivity — no public access needed
    - Note down: endpoint, port (5432), master username, master password
2. Connect to your RDS instance from the EC2 instance using the master credentials:
    
    ```bash
    psql postgres://master_user:master_password@your-rds-endpoint.rds.amazonaws.com:5432/postgres
    ```
    
3. Enable `vector` in `template1` so test databases inherit it:
    
    ```sql
    \c template1
    CREATE EXTENSION IF NOT EXISTS vector;
    \q
    ```
    

> On RDS, `rds_superuser` is the equivalent of superuser for extension management. Your master user already has it. In Step 5 you will grant it to your app user as well so it can install extensions in the temporary databases the test suite creates.

---

## Step 5 — Create a PostgreSQL User and Database

Generate a strong random password:

```bash
openssl rand -base64 24 | tr -d '/+=' | head -c 32
```

Copy the output. Now create the user and database.

**Local (Option A):**

```bash
sudo -u postgres psql <<EOF
CREATE USER your_username WITH PASSWORD 'your_password';
CREATE DATABASE your_database OWNER your_username;
GRANT ALL PRIVILEGES ON DATABASE your_database TO your_username;
ALTER USER your_username CREATEDB;
EOF
```

**RDS (Option B):** Connect as the master user and run:

```sql
CREATE USER your_username WITH PASSWORD 'your_password';
CREATE DATABASE your_database OWNER your_username;
GRANT ALL PRIVILEGES ON DATABASE your_database TO your_username;
ALTER USER your_username CREATEDB;
GRANT rds_superuser TO your_username;
```

> `CREATEDB` is required — the test suite spins up isolated temporary databases per test run.  
> `rds_superuser` (RDS only) allows the user to install the `vector` extension in those temporary databases.

---

## Step 6 — Configure wendRAG

wendRAG supports three configuration layers (lowest to highest priority):

1. **Environment variables** (`WEND_RAG_*`) — baseline
2. **`.env` file** — convenient for development; overrides env vars
3. **YAML config file** (`/etc/wend-rag/config.yaml`) — highest priority, ideal for production

For a production Ubuntu server, the recommended approach is the YAML config file.

### Create the config directory and file

```bash
sudo mkdir -p /etc/wend-rag
sudo cp config.example.yaml /etc/wend-rag/config.yaml
sudo nano /etc/wend-rag/config.yaml
```

Set these values:

```yaml
server:
  host: "0.0.0.0"
  port: 3000

storage:
  backend: "postgres"
  # Local:
  database_url: "postgres://your_username:your_password@localhost:5432/your_database"
  # RDS:
  # database_url: "postgres://your_username:your_password@your-rds-endpoint.rds.amazonaws.com:5432/your_database"

embedding:
  provider: "openai-compatible"
  api_key: "ollama"
  model: "bge-m3:latest"
  base_url: "http://localhost:11434/v1/"
  dimensions: 1024
```

Set restrictive file permissions:

```bash
sudo chmod 640 /etc/wend-rag/config.yaml
```

> **Note:** You can still use a `.env` file or `WEND_RAG_*` environment variables. Any value set in the YAML config file takes priority over both.

> **All environment variables use the `WEND_RAG_` prefix.** For example: `WEND_RAG_HOST`, `WEND_RAG_PORT`, `WEND_RAG_DATABASE_URL`, `WEND_RAG_EMBEDDING_PROVIDER`, etc.

---

## Step 7 — Build the Project

```bash
cargo build --release
```

The first build downloads and compiles all dependencies — this takes a few minutes. On a `c6i.xlarge` with 4 vCPUs, Cargo will parallelize the compilation automatically.

---

## Step 8 — Install the Binary System-Wide

Copy the compiled binary to `/usr/local/bin` so `wendRAG` is available from anywhere on the server:

```bash
sudo cp target/release/wend-rag /usr/local/bin/wendRAG
```

Verify it works:

```bash
wendRAG --help
```

You should see the CLI help output:

```
wendRAG — RAG-powered MCP server

Usage: wend-rag [OPTIONS] <COMMAND>

Commands:
  daemon        Start the RAG + MCP service over HTTP
  ingest        One-shot document ingestion, then exit
  stdio         Start the MCP server over stdio transport
  key:generate  Generate a new API key for HTTP transport authentication
  key:list      List registered API keys (name, prefix, created_at)
  key:revoke    Revoke an API key by name
  help          Print this message or the help of the given subcommand(s)

Options:
  -c, --config <CONFIG>  Path to YAML config file
  -h, --help             Print help
  -V, --version          Print version
```

---

## Step 9 — Run wendRAG as a Ubuntu Service

Create a systemd unit file:

```bash
sudo nano /etc/systemd/system/wendrag.service
```

Paste the following:

```ini
[Unit]
Description=wendRAG MCP Server
After=network.target

[Service]
Type=simple
User=ubuntu
ExecStart=/usr/local/bin/wendRAG daemon
Restart=on-failure
RestartSec=5
TimeoutStopSec=30

[Install]
WantedBy=multi-user.target
```

> If you are using **local PostgreSQL** (Option A), add `postgresql.service` to the `After=` line:
> 
> ```ini
> After=network.target postgresql.service
> ```

The server handles SIGTERM gracefully — when systemd sends the stop signal, in-flight HTTP requests are allowed to complete before the process exits. `TimeoutStopSec=30` gives the server up to 30 seconds to drain; after that systemd sends SIGKILL.

The systemd unit does not need an `EnvironmentFile=` directive — wendRAG reads its configuration from `/etc/wend-rag/config.yaml` automatically on Linux.

Enable and start the service:

```bash
sudo systemctl daemon-reload
sudo systemctl enable wendrag
sudo systemctl start wendrag
sudo systemctl status wendrag
```

Verify the health endpoint:

```bash
curl http://localhost:3000/health
# Expected: {"status":"ok"}
```

Check logs at any time:

```bash
journalctl -u wendrag -f
```

---

## Step 10 — Ingest Your Knowledge Base

Clone your knowledge base repository and ingest it:

```bash
cd ~/
git clone {your-knowledge-base-repository}
wendRAG ingest ~/{your-knowledge-base-repository}
```

wendRAG will recursively scan the directory for `.md`, `.txt`, and `.pdf` files, chunk and embed them, and load everything into the database. It prints a JSON summary when done and exits — the running service is unaffected.

To re-ingest after pulling updates:

```bash
cd ~/{your-knowledge-base-repository} && git pull
wendRAG ingest ~/{your-knowledge-base-repository}
```

Files whose content has not changed are automatically skipped (hash-based deduplication), so re-ingestion only processes what actually changed.

---

## Step 11 — Verify with Tests

```bash
TEST_DATABASE_URL=postgres://your_username:your_password@your_host:5432/your_database \
  cargo test --test backend_parity
```

Expected output (test count may grow over time):

```
running 9 tests
test backends_bootstrap_empty_store ... ok
test backends_match_ingest_and_search_behaviour ... ok
test backends_return_ordered_document_chunks ... ok
test backends_return_chunks_by_index ... ok
test graph_retrieval_works_on_all_backends ... ok
test incremental_sync_postgres ... ok
test incremental_sync_sqlite ... ok
test url_ingestion_respects_robots_txt ... ok
test url_ingestion_works_on_all_backends ... ok

test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured
```

---

## Step 12 — Generate an API Key (Recommended for Production)

When the server is reachable from the public internet, enable Bearer
token authentication before connecting any client. See
[authentication-setup.md](authentication-setup.md) for the full guide.

On the EC2 host, as the same user that runs the systemd service:

```bash
wendRAG key:generate --name prod-client
```

Copy the printed `Key:` value — it is the only time it will be shown.
The key is persisted as a SHA-256 hash at
`$HOME/.wend-rag/keys.json` (owner-read-only on Unix).

Restart the service so it loads the new keys file:

```bash
sudo systemctl restart wendrag
```

The daemon log will now include:

```
INFO wend_rag: API key authentication enabled on /mcp key_count=1
```

From this point on, unauthenticated requests to `/mcp` return `401
Unauthorized`. `/health` stays open for load-balancer probes.

---

## Step 13 — Connect an MCP Client

The server exposes MCP over HTTP at `http://your-ec2-public-ip:3000/mcp`.
See [mcp-client-setup.md](mcp-client-setup.md) for per-client config
(Cursor, Claude Desktop, VS Code, Codex, Windsurf, and raw curl).

The most common form is:

```json
{
  "mcpServers": {
    "wendRAG": {
      "type": "http",
      "url": "http://your-ec2-public-ip:3000/mcp",
      "headers": {
        "Authorization": "Bearer wrag_your_generated_key_here"
      }
    }
  }
}
```

> **Note**: You may send this file to your Claude Code then you can tell it to help you set it up

Or use stdio mode (for clients that launch the process directly):

```bash
wend-rag stdio
```

Quick smoke test with curl:

```bash
# /health never requires auth
curl http://your-ec2-public-ip:3000/health

# /mcp requires the key
curl -X POST http://your-ec2-public-ip:3000/mcp \
  -H "Authorization: Bearer wrag_your_generated_key_here" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
```

---

## Optional — Enable Entity Extraction + Graph Retrieval

Pull a local LLM via Ollama:

```bash
ollama pull llama3.2
```

Add to `/etc/wend-rag/config.yaml`:

```yaml
entity_extraction:
  enabled: true
  base_url: "http://localhost:11434"
  model: "llama3.2"

graph:
  enabled: true
  traversal_depth: 2
```

Restart the service:

```bash
sudo systemctl restart wendrag
```

Ingested documents will now have entities and relationships extracted and stored for graph-boosted retrieval.

---

## Environment Variable Reference

All environment variables use the `WEND_RAG_` prefix. The YAML config file takes priority over these. See [configuration.md](configuration.md) for the authoritative reference including memory, community, and reranker variables.

| Variable | Description | Default |
|---|---|---|
| `WEND_RAG_HOST` | Bind address | `0.0.0.0` |
| `WEND_RAG_PORT` | HTTP port | `3000` |
| `WEND_RAG_CONFIG` | Override YAML config file path | `/etc/wend-rag/config.yaml` |
| `WEND_RAG_API_KEY` | Optional static Bearer token for `/mcp` | *(unset — auth disabled)* |
| `WEND_RAG_KEYS_FILE` | Override path for the `keys.json` store | `$HOME/.wend-rag/keys.json` |
| `WEND_RAG_STORAGE_BACKEND` | `postgres` or `sqlite` | auto |
| `WEND_RAG_DATABASE_URL` | PostgreSQL connection URL | *(none)* |
| `WEND_RAG_SQLITE_PATH` | SQLite file path | `./wend-rag.db` |
| `WEND_RAG_EMBEDDING_PROVIDER` | `openai`, `voyage`, `ollama`, or `openai-compatible` | `openai` |
| `WEND_RAG_EMBEDDING_API_KEY` | API key for embedding service | *(empty)* |
| `WEND_RAG_EMBEDDING_BASE_URL` | Embedding API base URL | *(provider default)* |
| `WEND_RAG_EMBEDDING_MODEL` | Embedding model name | *(provider default)* |
| `WEND_RAG_EMBEDDING_DIMENSIONS` | Vector dimensions | *(provider default)* |
| `WEND_RAG_ENTITY_EXTRACTION_ENABLED` | Enable entity extraction | `false` |
| `WEND_RAG_ENTITY_EXTRACTION_LLM_URL` | LLM URL for extraction | *(falls back to embedding URL)* |
| `WEND_RAG_ENTITY_EXTRACTION_LLM_MODEL` | LLM model for extraction | `gpt-4.1-mini` |
| `WEND_RAG_ENTITY_EXTRACTION_API_KEY` | API key for extraction | *(falls back to embedding key)* |
| `WEND_RAG_GRAPH_RETRIEVAL_ENABLED` | Enable graph retrieval | `false` |
| `WEND_RAG_GRAPH_TRAVERSAL_DEPTH` | Graph traversal depth (1–3) | `2` |
| `WEND_RAG_COMMUNITY_LLM_SUMMARIES` | Enable LLM-generated community summaries | `false` |
| `WEND_RAG_COMMUNITY_LLM_URL` | Community summary endpoint | *(falls back to entity extraction URL)* |
| `WEND_RAG_COMMUNITY_LLM_MODEL` | Community summary model | *(falls back to entity extraction model)* |
| `WEND_RAG_COMMUNITY_LLM_API_KEY` | Community summary API key | *(falls back to entity extraction key)* |
| `WEND_RAG_RERANKER_ENABLED` | Enable reranker | `false` |
| `WEND_RAG_RERANKER_PROVIDER` | `cohere`, `jina`, or `openai-compatible` | `openai-compatible` |
| `WEND_RAG_RERANKER_BASE_URL` | Reranker endpoint | *(empty)* |
| `WEND_RAG_RERANKER_MODEL` | Reranker model name | `rerank-v3.5` |
| `WEND_RAG_RERANKER_API_KEY` | Reranker API key | *(falls back to embedding key)* |
| `WEND_RAG_CHUNKING_STRATEGY` | `fixed` or `semantic` | `fixed` |
| `WEND_RAG_CHUNKING_SEMANTIC_THRESHOLD` | Semantic breakpoint percentile | `0.25` |
| `WEND_RAG_CHUNKING_MAX_SENTENCES` | Max sentences per chunk | `20` |
| `WEND_RAG_CHUNKING_FILTER_GARBAGE` | Filter boilerplate before chunking | `true` |
| `WEND_RAG_MEMORY_ENABLED` | Enable the memory subsystem | `false` |
| `WEND_RAG_MEMORY_SESSION_TIMEOUT` | Session-buffer timeout (seconds) | `3600` |
| `WEND_RAG_MEMORY_DECAY_RATE` | Ebbinghaus decay α | `0.02` |
| `WEND_RAG_MEMORY_PRUNE_THRESHOLD` | Minimum importance retained | `0.3` |
| `WEND_RAG_MEMORY_MAX_PER_QUERY` | Max memories per `memory_retrieve` | `20` |
| `WEND_RAG_MEMORY_RECENCY_WEIGHT` | Relevance-vs-recency balance | `0.3` |
| `WEND_RAG_POOL_MAX_CONNECTIONS` | DB connection pool size | `20` |
| `WEND_RAG_POOL_ACQUIRE_TIMEOUT_SECS` | Pool acquire timeout (seconds) | `60` |

---

## Troubleshooting

|Error|Fix|
|---|---|
|`permission denied to create database`|`ALTER USER your_username CREATEDB;` as postgres/master superuser|
|`permission denied to create extension "vector"`|Local: run the `template1` command in Step 4A. RDS: `GRANT rds_superuser TO your_username;`|
|`connection refused` on port 5432|Local: `sudo systemctl start postgresql`. RDS: check the RDS security group allows port 5432 from the EC2 instance's security group|
|`connection refused` on port 3000|Check the EC2 security group allows inbound traffic on port 3000|
|`bge-m3 model not found`|`ollama pull bge-m3`|
|Build fails on `aws-lc-sys` / `ring`|`sudo apt install cmake libclang-dev`|
|Service fails to start|Check logs with `journalctl -u wendrag -f` and verify `/etc/wend-rag/config.yaml`|
|`wendRAG: command not found`|Re-run `sudo cp target/release/wend-rag /usr/local/bin/wendRAG`|
|Config file not loading|Verify the file exists at `/etc/wend-rag/config.yaml` and is valid YAML. Use `-c /path/to/config.yaml` to specify explicitly.|
|Old env vars not working|All env vars now require the `WEND_RAG_` prefix (e.g. `HOST` → `WEND_RAG_HOST`)|
