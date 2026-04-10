# CLI Reference

The `wend-rag` binary runs either the MCP server or performs one-shot ingestion depending on the flags provided. All environment settings (storage backend, embeddings, chunking) apply in both modes.

When running with Cargo, pass flags after `--` so they are not consumed by Cargo itself.

## Flags

| Flag | Description |
|------|-------------|
| *(none)* | Start the MCP server. Transport is controlled by `MCP_TRANSPORT` (default `http`). |
| `--stdio` | Force MCP **stdio** transport. Takes precedence over `MCP_TRANSPORT`. |
| `--ingest <path>` | **One-shot ingestion**: ingest a local file or `http(s)` URL, print a JSON summary to stdout, then exit. |
| `--ingest=<path>` | Same as `--ingest` with the path embedded in the argument. |

`--ingest` requires a path or URL immediately after the flag or after `=`.

## Examples

```bash
# HTTP server (default)
cargo run

# Stdio MCP — for clients that spawn the process directly
cargo run -- --stdio

# Ingest a local file
cargo run -- --ingest path/to/file.md
cargo run -- --ingest=path/to/file.md

# Ingest a remote URL
cargo run -- --ingest https://example.com/article
```

### One-shot ingestion with Docker Compose

Override the service command so the flags reach the binary rather than Docker:

```bash
docker compose run --rm wend-rag wend-rag --ingest /data/docs/some-note.md
```

Use a path that exists inside the container and matches the volume mounts declared in `compose.yml`.

## Ingestion JSON output

When `--ingest` completes, a JSON summary is printed to stdout:

```json
{
  "file_path": "path/to/file.md",
  "file_name": "file.md",
  "file_type": "markdown",
  "chunk_count": 12,
  "skipped": false
}
```

`skipped: true` means the document content hash was unchanged since the last ingestion and no work was done.
