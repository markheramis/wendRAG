use std::env;
use std::io::{self, ErrorKind};
use std::sync::Arc;

use wend_rag::config::{Config, EmbeddingProviderKind, StorageBackendKind, TransportMode};
use wend_rag::embed::{self, OpenAiCompatProvider};
use wend_rag::entity::{EntityExtractor, OpenAiCompatEntityExtractor};
use wend_rag::ingest::pipeline;
use wend_rag::mcp::server::{ServerConfig, WendRagServer};
use wend_rag::store;
use rmcp::ServiceExt;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tracing_subscriber::EnvFilter;

/**
 * Captures the supported one-shot CLI modes that run before the MCP server
 * transport is started.
 */
enum CliMode {
    Serve,
    Ingest { path: String },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .init();

    let cli_mode = parse_cli_mode()?;
    let cfg = Config::from_env()?;
    match &cli_mode {
        CliMode::Serve => {
            tracing::info!(
                transport = ?cfg.transport,
                backend = ?cfg.storage_backend,
                "starting wendRAG MCP server"
            )
        }
        CliMode::Ingest { path } => tracing::info!(path = %path, "starting one-shot ingestion"),
    }

    let storage = store::initialize_backend(&cfg).await?;

    let embedder: Arc<dyn embed::EmbeddingProvider> = Arc::new(OpenAiCompatProvider::new(
        cfg.embedding_base_url.clone(),
        cfg.embedding_api_key.clone(),
        cfg.embedding_model.clone(),
    ));
    let entity_extractor: Option<Arc<dyn EntityExtractor>> =
        cfg.entity_extraction_enabled.then(|| {
            Arc::new(OpenAiCompatEntityExtractor::new(
                cfg.entity_extraction_base_url.clone(),
                cfg.entity_extraction_api_key.clone(),
                cfg.entity_extraction_model.clone(),
            )) as Arc<dyn EntityExtractor>
        });

    if let CliMode::Ingest { path } = cli_mode {
        run_cli_ingest(
            &storage,
            &embedder,
            entity_extractor.as_ref(),
            &path,
            cfg.chunking_strategy,
            cfg.chunking_semantic_threshold,
        )
        .await?;
        return Ok(());
    }

    let server_config = ServerConfig {
        storage_backend: match cfg.storage_backend {
            StorageBackendKind::Postgres => "postgres",
            StorageBackendKind::Sqlite => "sqlite",
        }
        .to_string(),
        embedding_provider: match cfg.embedding_provider {
            EmbeddingProviderKind::OpenAi => "openai",
            EmbeddingProviderKind::Voyage => "voyage",
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
    };

    let server = WendRagServer::new(
        storage,
        embedder,
        entity_extractor,
        cfg.graph_settings,
        cfg.chunking_strategy,
        cfg.chunking_semantic_threshold,
        server_config,
    );

    match cfg.transport {
        TransportMode::Http => serve_http(server, &cfg.host, cfg.port).await,
        TransportMode::Stdio => serve_stdio(server).await,
    }
}

/**
 * Parses the top-level CLI flags that affect process mode while leaving the
 * existing MCP transport selection logic untouched.
 */
fn parse_cli_mode() -> Result<CliMode, io::Error> {
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        if let Some(path) = arg.strip_prefix("--ingest=") {
            return Ok(CliMode::Ingest {
                path: path.to_string(),
            });
        }

        if arg == "--ingest" {
            let path = args.next().ok_or_else(|| {
                io::Error::new(ErrorKind::InvalidInput, "--ingest requires a path argument")
            })?;
            return Ok(CliMode::Ingest { path });
        }
    }

    Ok(CliMode::Serve)
}

/**
 * Executes the one-shot CLI ingestion mode and writes a JSON summary to stdout
 * so shell users can inspect or pipe the result. Supports local paths and
 * HTTP(S) URLs.
 */
async fn run_cli_ingest(
    storage: &Arc<dyn store::StorageBackend>,
    embedder: &Arc<dyn embed::EmbeddingProvider>,
    entity_extractor: Option<&Arc<dyn EntityExtractor>>,
    path: &str,
    chunking_strategy: wend_rag::config::ChunkingStrategy,
    semantic_threshold: f64,
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
    )
    .await?;

    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}

/**
 * Starts the MCP server over Streamable HTTP transport (default).
 * Binds an Axum router on `host:port` with the MCP endpoint at `/mcp`.
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

    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    axum::serve(listener, router).await?;

    Ok(())
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
