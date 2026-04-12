# CLI Reference

The `wend-rag` binary exposes subcommands for serving the MCP server or performing one-shot ingestion. All environment settings (storage backend, embeddings, chunking) apply in every mode.

When running with Cargo, pass subcommands after `--` so they are not consumed by Cargo itself.

## Global Flags

| Flag | Description |
|------|-------------|
| `-c, --config <path>` | Path to a YAML config file. |

## Subcommands

| Subcommand | Description |
|------------|-------------|
| `daemon` | Start the MCP server over Streamable HTTP. |
| `stdio` | Start the MCP server over stdio transport. |
| `ingest <path>` | One-shot ingestion of a file, directory, or HTTP(S) URL, then exit. |

## Examples

```bash
# HTTP server (daemon mode)
cargo run -- daemon

# Stdio MCP — for clients that spawn the process directly
cargo run -- stdio

# Ingest a single file
cargo run -- ingest path/to/file.md

# Ingest an entire directory (recursive)
cargo run -- ingest path/to/docs/

# Ingest a remote URL
cargo run -- ingest https://example.com/article
```

### One-shot ingestion with Docker Compose

Override the service command so the subcommand reaches the binary rather than Docker:

```bash
docker compose run --rm wend-rag wend-rag ingest /data/docs/some-note.md
```

Use a path that exists inside the container and matches the volume mounts declared in `compose.yml`.

## Ingestion output

### Progress (stderr)

During ingestion, per-file status logs are written to **stderr** via the `tracing` framework so you can follow progress in real time. Example output:

```
2026-04-10T12:00:00.000Z  INFO wend_rag::ingest::directory: discovered files for ingestion directory="docs/" file_count=3
2026-04-10T12:00:00.010Z  INFO wend_rag::ingest::directory: ingesting file="docs/intro.md" progress="[1/3]"
2026-04-10T12:00:01.200Z  INFO wend_rag::ingest::directory: done file="docs/intro.md" status="created" chunks=5
2026-04-10T12:00:01.210Z  INFO wend_rag::ingest::directory: ingesting file="docs/setup.md" progress="[2/3]"
2026-04-10T12:00:02.100Z  INFO wend_rag::ingest::directory: done file="docs/setup.md" status="unchanged" chunks=0
2026-04-10T12:00:02.110Z  INFO wend_rag::ingest::directory: ingesting file="docs/bad.xyz" progress="[3/3]"
2026-04-10T12:00:02.120Z ERROR wend_rag::ingest::directory: failed file="docs/bad.xyz" error="unsupported file type: xyz"
2026-04-10T12:00:02.130Z  INFO wend_rag: ingestion complete added=1 updated=0 unchanged=1 deleted=0 failed=1
```

Control the log level with the `RUST_LOG` environment variable (default: `info`).

### JSON summary (stdout)

When ingestion completes, a JSON summary is printed to **stdout**:

```json
{
  "added": 1,
  "updated": 0,
  "unchanged": 1,
  "deleted": 0,
  "failed": 1,
  "documents": [
    { "file_path": "docs/intro.md", "status": "created" },
    { "file_path": "docs/setup.md", "status": "unchanged" },
    { "file_path": "docs/bad.xyz", "status": "error: unsupported file type: xyz" }
  ]
}
```

This makes it safe to pipe stdout into `jq` or another consumer while still watching progress on stderr.
