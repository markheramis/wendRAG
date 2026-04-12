use std::sync::Arc;

use axum::response::IntoResponse;
use clap::{Parser, Subcommand};
use wend_rag::config::{Config, EmbeddingProviderKind, StorageBackendKind};
use wend_rag::config_file::FileConfig;
use wend_rag::embed::{self, OllamaProvider, OpenAiCompatProvider};
use wend_rag::entity::{EntityExtractor, OpenAiCompatEntityExtractor};
use wend_rag::ingest::pipeline;
use wend_rag::mcp::server::{ServerConfig, WendRagServer};
use wend_rag::observability;
use wend_rag::rerank::{self, RerankerProvider};
use wend_rag::store;
use rmcp::ServiceExt;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};

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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize observability with OpenTelemetry support
    observability::init_tracing();

    let cli = Cli::parse();

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
            let server = build_server(cfg, storage, embedder, entity_extractor, reranker);
            serve_http(server, &host, port).await
        }
        Command::Stdio => {
            let server = build_server(cfg, storage, embedder, entity_extractor, reranker);
            serve_stdio(server).await
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
 */
async fn serve_http(
    server: WendRagServer,
    host: &str,
    port: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    let bind_addr = format!("{host}:{port}");
    tracing::info!(addr = %bind_addr, "MCP Streamable HTTP server listening at /mcp");

    let service: StreamableHttpService<WendRagServer, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(server.clone()),
            Default::default(),
            StreamableHttpServerConfig::default(),
        );

    let router = axum::Router::new()
        .route("/health", axum::routing::get(health_handler))
        .nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("server shut down gracefully");
    Ok(())
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
