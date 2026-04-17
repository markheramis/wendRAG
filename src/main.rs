use std::io::{self, BufRead, Write};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::middleware::{self as axum_middleware, Next};
use axum::response::{IntoResponse, Response};
use clap::{Parser, Subcommand};
use rmcp::ServiceExt;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use wend_rag::auth::{Authenticator, KeyStore, default_keys_path};
use wend_rag::config::{Config, EmbeddingProviderKind, StorageBackendKind};
use wend_rag::config_file::FileConfig;
use wend_rag::embed::{self, OllamaProvider, OpenAiCompatProvider};
use wend_rag::entity::{EntityExtractor, OpenAiCompatEntityExtractor};
use wend_rag::ingest::pipeline;
use wend_rag::mcp::server::{ServerConfig, WendRagServer};
use wend_rag::memory;
use wend_rag::observability;
use wend_rag::rerank::{self, RerankerProvider};
use wend_rag::store;

#[derive(Parser)]
#[command(name = "wend-rag", version, about = "wendRAG — RAG-powered MCP server")]
struct Cli {
    /// Path to YAML config file (default: /etc/wend-rag/config.yaml on Linux)
    #[arg(short, long, global = true)]
    config: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the RAG + MCP service over HTTP
    Daemon,
    /// One-shot document ingestion, then exit
    Ingest {
        /// Path to file, directory, or URL to ingest
        path: String,
    },
    /// Start the MCP server over stdio transport
    Stdio,
    /// Generate a new API key for HTTP transport authentication.
    /// Prompts for a human-readable name, then prints the raw key exactly
    /// once. Only a SHA-256 hash is persisted to disk.
    #[command(name = "key:generate")]
    KeyGenerate {
        /// Optional key name. When omitted, the command prompts interactively.
        #[arg(long)]
        name: Option<String>,
    },
    /// List registered API keys by name, display prefix, and creation time.
    /// The underlying keys are never shown; only their non-sensitive metadata.
    #[command(name = "key:list")]
    KeyList,
    /// Revoke an API key by name. Once revoked the key's hash is removed and
    /// any client still presenting it will receive 401 Unauthorized.
    #[command(name = "key:revoke")]
    KeyRevoke {
        /// Name of the key to revoke. When omitted, the command prompts.
        name: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize observability with OpenTelemetry support
    observability::init_tracing();

    let cli = Cli::parse();

    // Key management commands intentionally skip the full runtime bootstrap
    // (storage, embedder, memory) so operators can manage keys on a host
    // that doesn't have the data plane configured yet.
    match cli.command {
        Command::KeyGenerate { name } => return run_key_generate(name),
        Command::KeyList => return run_key_list(),
        Command::KeyRevoke { name } => return run_key_revoke(name),
        _ => {}
    }

    let file_config = FileConfig::load(cli.config.as_deref());
    let cfg = Config::load(file_config.as_ref())?;

    match &cli.command {
        Command::Daemon => {
            tracing::info!(
                backend = ?cfg.storage_backend,
                "starting wendRAG MCP server (daemon)"
            )
        }
        Command::Ingest { path } => {
            tracing::info!(path = %path, "starting one-shot ingestion")
        }
        Command::Stdio => {
            tracing::info!("starting wendRAG MCP server (stdio)")
        }
        Command::KeyGenerate { .. } | Command::KeyList | Command::KeyRevoke { .. } => {
            unreachable!("key subcommands are dispatched above")
        }
    }

    let storage = store::initialize_backend(&cfg).await?;

    // Initialize embedder based on provider kind
    let embedder: Arc<dyn embed::EmbeddingProvider> = match cfg.embedding_provider {
        EmbeddingProviderKind::Ollama => Arc::new(OllamaProvider::new(
            cfg.embedding_base_url.clone(),
            cfg.embedding_model.clone(),
        )),
        _ => Arc::new(OpenAiCompatProvider::new(
            cfg.embedding_base_url.clone(),
            cfg.embedding_api_key.clone(),
            cfg.embedding_model.clone(),
        )),
    };
    let entity_extractor: Option<Arc<dyn EntityExtractor>> =
        cfg.entity_extraction_enabled.then(|| {
            Arc::new(OpenAiCompatEntityExtractor::new(
                cfg.entity_extraction_base_url.clone(),
                cfg.entity_extraction_api_key.clone(),
                cfg.entity_extraction_model.clone(),
            )) as Arc<dyn EntityExtractor>
        });

    let reranker: Option<Arc<dyn RerankerProvider>> = cfg.reranker.enabled.then(|| {
        tracing::info!(
            provider = ?cfg.reranker.provider,
            model = %cfg.reranker.model,
            top_n = cfg.reranker.top_n,
            "reranker enabled"
        );
        Arc::from(rerank::build_reranker(&cfg.reranker)) as Arc<dyn RerankerProvider>
    });

    let memory_manager: Option<Arc<memory::MemoryManager>> = if cfg.memory.enabled {
        let mem_storage: Arc<dyn memory::MemoryStorage> = match cfg.storage_backend {
            StorageBackendKind::Postgres => {
                let db_url = cfg.database_url.as_deref().unwrap_or("");
                let pool = sqlx::postgres::PgPoolOptions::new()
                    .max_connections(cfg.pool.max_connections)
                    .acquire_timeout(cfg.pool.acquire_timeout)
                    .connect(db_url)
                    .await?;
                Arc::new(memory::PostgresMemoryStorage::new(pool))
            }
            StorageBackendKind::Sqlite => {
                let pool = store::connect_sqlite_pool(&cfg.sqlite_path, &cfg.pool).await?;
                Arc::new(memory::SqliteMemoryStorage::new(pool))
            }
        };
        tracing::info!("memory subsystem enabled");
        Some(Arc::new(memory::MemoryManager::new(
            cfg.memory.clone(),
            mem_storage,
            embedder.clone(),
        )))
    } else {
        None
    };

    match cli.command {
        Command::Ingest { path } => {
            run_cli_ingest(
                &storage,
                &embedder,
                entity_extractor.as_ref(),
                &path,
                cfg.chunking_strategy,
                cfg.chunking_semantic_threshold,
                cfg.chunking_max_sentences,
                cfg.chunking_filter_garbage,
            )
            .await?;
            Ok(())
        }
        Command::Daemon => {
            let host = cfg.host.clone();
            let port = cfg.port;
            if let Some(ref mm) = memory_manager {
                let interval = std::time::Duration::from_secs(
                    (cfg.memory.consolidation_interval_hours * 3600).max(60) as u64,
                );
                memory::maintenance::spawn_maintenance_task(Arc::clone(mm), interval);
            }
            let authenticator = Arc::new(Authenticator::from_environment()?);
            let server = build_server(cfg, storage, embedder, entity_extractor, reranker, memory_manager);
            serve_http(server, &host, port, authenticator).await
        }
        Command::Stdio => {
            if let Some(ref mm) = memory_manager {
                let interval = std::time::Duration::from_secs(
                    (cfg.memory.consolidation_interval_hours * 3600).max(60) as u64,
                );
                memory::maintenance::spawn_maintenance_task(Arc::clone(mm), interval);
            }
            let server = build_server(cfg, storage, embedder, entity_extractor, reranker, memory_manager);
            serve_stdio(server).await
        }
        Command::KeyGenerate { .. } | Command::KeyList | Command::KeyRevoke { .. } => {
            unreachable!("key subcommands are dispatched above")
        }
    }
}

/**
 * Constructs the `WendRagServer` with its public `ServerConfig` snapshot.
 * Shared by both the daemon and stdio code paths.
 */
fn build_server(
    cfg: Config,
    storage: Arc<dyn store::StorageBackend>,
    embedder: Arc<dyn embed::EmbeddingProvider>,
    entity_extractor: Option<Arc<dyn EntityExtractor>>,
    reranker: Option<Arc<dyn RerankerProvider>>,
    memory_manager: Option<Arc<memory::MemoryManager>>,
) -> WendRagServer {
    let server_config = ServerConfig {
        storage_backend: match cfg.storage_backend {
            StorageBackendKind::Postgres => "postgres",
            StorageBackendKind::Sqlite => "sqlite",
        }
        .to_string(),
        embedding_provider: match cfg.embedding_provider {
            EmbeddingProviderKind::OpenAi => "openai",
            EmbeddingProviderKind::Voyage => "voyage",
            EmbeddingProviderKind::Ollama => "ollama",
            EmbeddingProviderKind::OpenAiCompatible => "openai-compatible",
        }
        .to_string(),
        embedding_model: cfg.embedding_model.clone(),
        embedding_dimensions: cfg.embedding_dimensions,
        entity_extraction_enabled: cfg.entity_extraction_enabled,
        graph_retrieval_enabled: cfg.graph_settings.enabled,
        graph_traversal_depth: cfg.graph_settings.traversal_depth,
        chunking_strategy: match cfg.chunking_strategy {
            wend_rag::config::ChunkingStrategy::Fixed => "fixed",
            wend_rag::config::ChunkingStrategy::Semantic => "semantic",
        }
        .to_string(),
        chunking_semantic_threshold: cfg.chunking_semantic_threshold,
        chunking_max_sentences: cfg.chunking_max_sentences,
        chunking_filter_garbage: cfg.chunking_filter_garbage,
        reranker_enabled: cfg.reranker.enabled,
        reranker_provider: match cfg.reranker.provider {
            wend_rag::rerank::RerankerProviderKind::Cohere => "cohere",
            wend_rag::rerank::RerankerProviderKind::Jina => "jina",
            wend_rag::rerank::RerankerProviderKind::OpenAiCompatible => "openai-compatible",
        }
        .to_string(),
        reranker_model: cfg.reranker.model.clone(),
    };

    WendRagServer::new(
        storage,
        embedder,
        entity_extractor,
        reranker,
        cfg.reranker.top_n,
        cfg.graph_settings,
        cfg.community.clone(),
        memory_manager,
        cfg.chunking_strategy,
        cfg.chunking_semantic_threshold,
        cfg.chunking_max_sentences,
        cfg.chunking_filter_garbage,
        server_config,
    )
}

/**
 * Executes the one-shot CLI ingestion mode and writes a JSON summary to stdout
 * so shell users can inspect or pipe the result. Logs per-file progress and an
 * aggregate summary to stderr. Supports local paths and HTTP(S) URLs.
 */
#[allow(clippy::too_many_arguments)]
async fn run_cli_ingest(
    storage: &Arc<dyn store::StorageBackend>,
    embedder: &Arc<dyn embed::EmbeddingProvider>,
    entity_extractor: Option<&Arc<dyn EntityExtractor>>,
    path: &str,
    chunking_strategy: wend_rag::config::ChunkingStrategy,
    semantic_threshold: f64,
    max_sentences: usize,
    filter_garbage: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let tags: Vec<String> = Vec::new();
    let output = pipeline::ingest_path(
        storage,
        embedder,
        entity_extractor,
        path,
        None,
        &tags,
        chunking_strategy,
        semantic_threshold,
        max_sentences,
        filter_garbage,
    )
    .await?;

    tracing::info!(
        added = output.added,
        updated = output.updated,
        unchanged = output.unchanged,
        deleted = output.deleted,
        failed = output.failed,
        "ingestion complete"
    );

    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}

/**
 * Starts the MCP server over Streamable HTTP transport (daemon mode).
 * Binds an Axum router on `host:port` with the MCP endpoint at `/mcp` and a
 * plain `/health` endpoint for systemd, load-balancers, and container probes.
 * Listens for SIGTERM / ctrl_c and drains in-flight requests before exiting.
 *
 * When `authenticator.is_auth_required()` returns true, all requests to
 * `/mcp` must carry an `Authorization: Bearer <token>` header matching one
 * of the registered keys (either from `WEND_RAG_API_KEY` or the keys file).
 * The `/health` endpoint is intentionally left unauthenticated so probes
 * do not need to carry credentials.
 */
async fn serve_http(
    server: WendRagServer,
    host: &str,
    port: u16,
    authenticator: Arc<Authenticator>,
) -> Result<(), Box<dyn std::error::Error>> {
    let bind_addr = format!("{host}:{port}");
    tracing::info!(addr = %bind_addr, "MCP Streamable HTTP server listening at /mcp");

    let service: StreamableHttpService<WendRagServer, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(server.clone()),
            Default::default(),
            StreamableHttpServerConfig::default(),
        );

    let mut mcp_router = axum::Router::new().nest_service("/mcp", service);
    if authenticator.is_auth_required() {
        tracing::info!(
            key_count = authenticator.key_count(),
            "API key authentication enabled on /mcp"
        );
        mcp_router = mcp_router.layer(axum_middleware::from_fn_with_state(
            authenticator.clone(),
            auth_middleware,
        ));
    } else {
        tracing::warn!(
            "API key authentication DISABLED -- /mcp accepts unauthenticated requests. \
             Run `wend-rag key:generate` or set WEND_RAG_API_KEY to enable."
        );
    }

    let router = axum::Router::new()
        .route("/health", axum::routing::get(health_handler))
        .merge(mcp_router);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("server shut down gracefully");
    Ok(())
}

/**
 * Axum middleware that enforces Bearer token authentication on protected
 * routes. Extracts the `Authorization` header, strips the `Bearer ` prefix,
 * and defers to [`Authenticator::validate`] for constant-time comparison.
 *
 * Returns 401 Unauthorized on any mismatch or missing header.
 */
async fn auth_middleware(
    State(auth): State<Arc<Authenticator>>,
    headers: HeaderMap,
    request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let token = headers
        .get("Authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty());

    match token {
        Some(presented) if auth.validate(presented) => Ok(next.run(request).await),
        _ => {
            tracing::warn!("rejecting request to /mcp: missing or invalid bearer token");
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}

/**
 * Returns a 200 OK JSON response for health probes. Intentionally lightweight
 * — it confirms the HTTP listener is alive without touching the database.
 */
async fn health_handler() -> impl IntoResponse {
    axum::Json(serde_json::json!({ "status": "ok" }))
}

/**
 * Waits for either ctrl_c (SIGINT) or SIGTERM, whichever arrives first, so
 * `axum::serve` can begin its graceful shutdown sequence.
 * Also shuts down OpenTelemetry tracing gracefully.
 */
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to register SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => tracing::info!("received SIGINT, shutting down"),
            _ = sigterm.recv() => tracing::info!("received SIGTERM, shutting down"),
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await.ok();
        tracing::info!("received SIGINT, shutting down");
    }

    // Flush OpenTelemetry spans before exiting
    observability::shutdown_tracing();
}

/**
 * Starts the MCP server over stdio transport.
 * Reads JSON-RPC from stdin, writes to stdout. Tracing goes to stderr.
 */
async fn serve_stdio(server: WendRagServer) -> Result<(), Box<dyn std::error::Error>> {
    tracing::info!("MCP stdio server starting on stdin/stdout");

    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .inspect_err(|error| tracing::error!("stdio serve error: {:?}", error))?;

    service.waiting().await?;

    tracing::info!("MCP stdio server shut down");
    Ok(())
}

/**
 * Runs the `key:generate` subcommand.
 *
 * When `name` is `None` the command prompts interactively on stdout/stdin
 * so operators can use it ad-hoc. The generated key is printed to stdout
 * exactly once; only its SHA-256 hash is persisted.
 */
fn run_key_generate(name: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let name = match name {
        Some(n) => n,
        None => prompt_line("Enter key name: ")?,
    };

    let mut store = KeyStore::load_default()?;
    println!("Generating...\n");
    let raw = store.add_key(&name)?;
    store.save_default()?;

    let stored = store
        .keys()
        .iter()
        .find(|k| k.name == name.trim())
        .expect("just-added key must be present");

    let path = default_keys_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<unset>".to_string());

    println!("Key Created");
    println!();
    println!("Name:       {}", stored.name);
    println!("Key:        {raw}");
    println!("Prefix:     {}", stored.key_prefix);
    println!("Created at: {}", stored.created_at.to_rfc3339());
    println!("Stored in:  {path}");
    println!();
    println!("Keep this key safe -- it will not be shown again.");
    println!(
        "Use it with MCP clients by setting `Authorization: Bearer {}` on /mcp.",
        stored.key_prefix,
    );
    Ok(())
}

/**
 * Runs the `key:list` subcommand. Prints each registered key's non-sensitive
 * metadata (name, display prefix, creation time).
 */
fn run_key_list() -> Result<(), Box<dyn std::error::Error>> {
    let store = KeyStore::load_default()?;
    if store.is_empty() {
        println!("No keys registered.");
        if let Some(path) = default_keys_path() {
            println!("Keys file: {}", path.display());
        }
        println!("Run `wend-rag key:generate` to create one.");
        return Ok(());
    }

    println!("{:<24} {:<20} CREATED", "NAME", "PREFIX");
    for key in store.keys() {
        println!(
            "{:<24} {:<20} {}",
            key.name,
            key.key_prefix,
            key.created_at.to_rfc3339()
        );
    }
    Ok(())
}

/**
 * Runs the `key:revoke` subcommand. Removes the named key's hash from the
 * store; subsequent requests presenting that key will receive 401.
 */
fn run_key_revoke(name: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let name = match name {
        Some(n) => n,
        None => prompt_line("Name of the key to revoke: ")?,
    };

    let mut store = KeyStore::load_default()?;
    store.revoke(&name)?;
    store.save_default()?;

    println!("Key '{}' revoked.", name.trim());
    Ok(())
}

/**
 * Reads a single line from stdin after writing a prompt to stdout. The
 * returned string has leading/trailing whitespace stripped so interactive
 * use is ergonomic.
 */
fn prompt_line(prompt: &str) -> io::Result<String> {
    print!("{prompt}");
    io::stdout().flush()?;
    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    Ok(line.trim().to_string())
}
