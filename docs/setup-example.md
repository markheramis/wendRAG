# Setup Example (wendRAG with PostgreSQL + Ollama on Ubuntu)

## Target Environment

This tutorial sets up wendRAG on an **AWS EC2 `c6i.xlarge`** instance running **Ubuntu 24.04 LTS**.

**Instance Type**: c6i.xlarge (4 vCPU, 8 GB RAM)
**Operating System**: Ubuntu 24.04 LTS
**Storage backend**: PostgreSQL 16 + pgvector (local or AWS RDS)
**Embedding model**: bge-m3 via Ollama (local, no API key required)

When launching your EC2 instance, ensure your security group allows:

- **Port 22** — SSH access from your IP
- **Port 3000** — wendRAG MCP server (from your application or MCP client)
- **Port 5432** — PostgreSQL (only if using local PostgreSQL and connecting remotely; skip if RDS handles this separately)

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

Pre-install the `vector` extension in `template1` so every database created later inherits it automatically — required for migrations to run as a non-superuser:

```bash
sudo -u postgres psql -d template1 -c "CREATE EXTENSION IF NOT EXISTS vector;"
```

Then proceed to **Step 5** and use `localhost:5432` as your host.

---

### Option B — AWS RDS

**Skip installation entirely.** Instead:

1. Create an RDS instance in the AWS console with:
    
    - Engine: **PostgreSQL 16** (pgvector is built in on RDS PostgreSQL 14.5+)
    - Place it in the **same VPC as your EC2 instance** for private connectivity — no public access needed
    - Note down: endpoint, port (5432), master username, master password
2. Connect to your RDS instance from the EC2 instance using the master credentials:
    
    ```bash
    psql postgres://master_user:master_password@your-rds-endpoint.rds.amazonaws.com:5432/postgres
    ```
    
3. Enable `vector` in `template1` so test databases inherit it:
    
    ```sql
    \c template1
    CREATE EXTENSION IF NOT EXISTS vector;
    \q
    ```
    

> On RDS, `rds_superuser` is the equivalent of superuser for extension management. Your master user already has it. In Step 5 you will grant it to your app user as well so it can install extensions in the temporary databases the test suite creates.

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

**RDS (Option B):** Connect as the master user and run:

```sql
CREATE USER your_username WITH PASSWORD 'your_password';
CREATE DATABASE your_database OWNER your_username;
GRANT ALL PRIVILEGES ON DATABASE your_database TO your_username;
ALTER USER your_username CREATEDB;
GRANT rds_superuser TO your_username;
```

> `CREATEDB` is required — the test suite spins up isolated temporary databases per test run.  
> `rds_superuser` (RDS only) allows the user to install the `vector` extension in those temporary databases.

---

## Step 6 — Configure the Environment

```bash
cp .env.example .env
nano .env
```

Set these values:

```dotenv
HOST=0.0.0.0
PORT=3000

STORAGE_BACKEND=postgres

# Local:
DATABASE_URL=postgres://your_username:your_password@localhost:5432/your_database

# RDS:
# DATABASE_URL=postgres://your_username:your_password@your-rds-endpoint.rds.amazonaws.com:5432/your_database

EMBEDDING_PROVIDER=openai-compatible
EMBEDDING_API_KEY=ollama
EMBEDDING_MODEL=bge-m3:latest
EMBEDDING_BASE_URL=http://localhost:11434/v1/
EMBEDDING_DIMENSIONS=1024
```

Leave everything else commented out for a basic setup.

---

## Step 7 — Build the Project

```bash
cargo build --release
```

The first build downloads and compiles all dependencies — this takes a few minutes. On a `c6i.xlarge` with 4 vCPUs, Cargo will parallelize the compilation automatically.

---

## Step 8 — Install the Binary System-Wide

Copy the compiled binary to `/usr/local/bin` so `wendRAG` is available from anywhere on the server:

```bash
sudo cp target/release/wend-rag /usr/local/bin/wendRAG
```

Verify it works:

```bash
wendRAG --help
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
WorkingDirectory=/home/ubuntu/wendRAG
EnvironmentFile=/home/ubuntu/wendRAG/.env
ExecStart=/usr/local/bin/wendRAG
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

> If you are using **local PostgreSQL** (Option A), add `postgresql.service` to the `After=` line:
> 
> ```ini
> After=network.target postgresql.service
> ```

Enable and start the service:

```bash
sudo systemctl daemon-reload
sudo systemctl enable wendrag
sudo systemctl start wendrag
sudo systemctl status wendrag
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
wendRAG --ingest ~/{your-knowledge-base-repository}
```

wendRAG will recursively scan the directory for `.md`, `.txt`, and `.pdf` files, chunk and embed them, and load everything into the database. It prints a JSON summary when done and exits — the running service is unaffected.

To re-ingest after pulling updates:

```bash
cd ~/{your-knowledge-base-repository} && git pull
wendRAG --ingest ~/{your-knowledge-base-repository}
```

Files whose content has not changed are automatically skipped (hash-based deduplication), so re-ingestion only processes what actually changed.

---

## Step 11 — Verify with Tests

```bash
TEST_DATABASE_URL=postgres://your_username:your_password@your_host:5432/your_database \
  cargo test --test backend_parity
```

Expected output:

```
running 6 tests
test backends_bootstrap_empty_store ... ok
test backends_match_ingest_and_search_behaviour ... ok
test backends_return_ordered_document_chunks_for_full_context ... ok
test graph_retrieval_works_on_all_backends ... ok
test url_ingestion_respects_robots_txt ... ok
test url_ingestion_works_on_all_backends ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured
```

---

## Step 12 — Connect an MCP Client

The server exposes MCP over HTTP at `http://your-ec2-public-ip:3000/mcp`. Add to your MCP client config:

```json
{
  "mcpServers": {
    "code-rag": {
      "type": "http",
      "url": "http://your-ec2-public-ip:3000/mcp"
    }
  }
}
```

> **Note**: You may send this file to your Claude Code then you can tell it to help you set it up

Or use stdio mode (for clients that launch the process directly):

```bash
./target/release/wend-rag --stdio
```

The `mcp.json` in the repo root has ready-made entries for both transports.

---

## Optional — Enable Entity Extraction + Graph Retrieval

Pull a local LLM via Ollama:

```bash
ollama pull llama3.2
```

Add to `.env`:

```dotenv
ENTITY_EXTRACTION_ENABLED=true
GRAPH_RETRIEVAL_ENABLED=true
GRAPH_TRAVERSAL_DEPTH=2
ENTITY_EXTRACTION_LLM_URL=http://localhost:11434
ENTITY_EXTRACTION_LLM_MODEL=llama3.2
```

Restart the service:

```bash
sudo systemctl restart wendrag
```

Ingested documents will now have entities and relationships extracted and stored for graph-boosted retrieval.

---

## Troubleshooting

|Error|Fix|
|---|---|
|`permission denied to create database`|`ALTER USER your_username CREATEDB;` as postgres/master superuser|
|`permission denied to create extension "vector"`|Local: run the `template1` command in Step 4A. RDS: `GRANT rds_superuser TO your_username;`|
|`connection refused` on port 5432|Local: `sudo systemctl start postgresql`. RDS: check the RDS security group allows port 5432 from the EC2 instance's security group|
|`connection refused` on port 3000|Check the EC2 security group allows inbound traffic on port 3000|
|`bge-m3 model not found`|`ollama pull bge-m3`|
|Build fails on `aws-lc-sys` / `ring`|`sudo apt install cmake libclang-dev`|
|Service fails to start|Check logs with `journalctl -u wendrag -f` and verify the `.env` path in the unit file|
|`wendRAG: command not found`|Re-run `sudo cp target/release/wend-rag /usr/local/bin/wendRAG`|