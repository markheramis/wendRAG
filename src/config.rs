use std::env;
use std::time::Duration;

use crate::entity::{DEFAULT_GRAPH_TRAVERSAL_DEPTH, GraphSettings};

const DEFAULT_POOL_MAX_CONNECTIONS: u32 = 20;
const DEFAULT_POOL_ACQUIRE_TIMEOUT_SECS: u64 = 60;

#[derive(Debug, Clone)]
pub enum EmbeddingProviderKind {
    OpenAi,
    Voyage,
    OpenAiCompatible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageBackendKind {
    Postgres,
    Sqlite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportMode {
    Http,
    Stdio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkingStrategy {
    Fixed,
    Semantic,
}

impl ChunkingStrategy {
    /**
     * Parses the configured chunking strategy while preserving the existing
     * "fixed on unknown input" behavior.
     */
    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "semantic" => Self::Semantic,
            _ => Self::Fixed,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub transport: TransportMode,
    pub host: String,
    pub port: u16,
    pub storage_backend: StorageBackendKind,
    pub database_url: Option<String>,
    pub sqlite_path: String,
    pub embedding_api_key: String,
    pub embedding_base_url: String,
    pub embedding_model: String,
    #[allow(dead_code)]
    pub embedding_provider: EmbeddingProviderKind,
    #[allow(dead_code)]
    pub embedding_dimensions: usize,
    pub entity_extraction_enabled: bool,
    pub entity_extraction_base_url: String,
    pub entity_extraction_model: String,
    pub entity_extraction_api_key: String,
    pub graph_settings: GraphSettings,
    pub chunking_strategy: ChunkingStrategy,
    /// Percentile (0.0..1.0) below which similarity scores become chunk breaks.
    /// E.g. 0.25 means the bottom 25% of consecutive-sentence similarities are break points.
    pub chunking_semantic_threshold: f64,
    pub pool: PoolConfig,
}

/**
 * Connection pool tuning knobs shared by both the PostgreSQL and SQLite
 * backends. Parsed from `POOL_MAX_CONNECTIONS` and `POOL_ACQUIRE_TIMEOUT_SECS`
 * environment variables with safe production defaults.
 */
#[derive(Debug, Clone, Copy)]
pub struct PoolConfig {
    pub max_connections: u32,
    pub acquire_timeout: Duration,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_connections: DEFAULT_POOL_MAX_CONNECTIONS,
            acquire_timeout: Duration::from_secs(DEFAULT_POOL_ACQUIRE_TIMEOUT_SECS),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("missing required env var: {0}")]
    MissingVar(#[from] env::VarError),
    #[error("invalid port number: {0}")]
    InvalidPort(#[from] std::num::ParseIntError),
    #[error("unknown embedding provider: {0} (expected openai, voyage, or openai-compatible)")]
    InvalidProvider(String),
    #[error("unknown storage backend: {0} (expected postgres or sqlite)")]
    InvalidStorageBackend(String),
    #[error("DATABASE_URL is required when STORAGE_BACKEND=postgres")]
    MissingDatabaseUrlForPostgres,
}

impl Config {
    /**
     * Determines the transport mode from CLI args and environment.
     * Priority: `--stdio` CLI flag > `MCP_TRANSPORT` env var > default (Http).
     */
    fn resolve_transport(args: &[String], transport_env: Option<&str>) -> TransportMode {
        if args.iter().any(|arg| arg == "--stdio") {
            return TransportMode::Stdio;
        }

        match transport_env {
            Some("stdio") => TransportMode::Stdio,
            _ => TransportMode::Http,
        }
    }

    /**
     * Resolves the active storage backend from explicit configuration first,
     * then falls back to the presence of a PostgreSQL URL.
     */
    fn resolve_storage_backend(
        explicit_backend: Option<&str>,
        has_database_url: bool,
    ) -> Result<StorageBackendKind, ConfigError> {
        match explicit_backend {
            Some("postgres") => Ok(StorageBackendKind::Postgres),
            Some("sqlite") => Ok(StorageBackendKind::Sqlite),
            Some(other) => Err(ConfigError::InvalidStorageBackend(other.to_string())),
            None if has_database_url => Ok(StorageBackendKind::Postgres),
            None => Ok(StorageBackendKind::Sqlite),
        }
    }

    /**
     * Parses a relaxed boolean environment variable, accepting common truthy
     * values while treating everything else as false.
     */
    fn parse_env_bool(name: &str, default: bool) -> bool {
        env::var(name)
            .ok()
            .map(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(default)
    }

    /**
     * Parses the graph traversal depth and clamps it into the supported range
     * used by the PostgreSQL recursive CTE implementation.
     */
    fn parse_graph_settings() -> GraphSettings {
        let enabled = Self::parse_env_bool("GRAPH_RETRIEVAL_ENABLED", false);
        let traversal_depth = env::var("GRAPH_TRAVERSAL_DEPTH")
            .ok()
            .and_then(|value| value.parse::<u8>().ok())
            .unwrap_or(DEFAULT_GRAPH_TRAVERSAL_DEPTH);
        GraphSettings::new(enabled, traversal_depth)
    }

    /**
     * Loads runtime configuration from environment variables after optionally
     * reading a local `.env` file.
     */
    pub fn from_env() -> Result<Self, ConfigError> {
        dotenvy::dotenv().ok();

        let args: Vec<String> = env::args().collect();
        let transport = Self::resolve_transport(&args, env::var("MCP_TRANSPORT").ok().as_deref());

        let provider_str = env::var("EMBEDDING_PROVIDER").unwrap_or_else(|_| "openai".into());
        let provider = match provider_str.as_str() {
            "openai" => EmbeddingProviderKind::OpenAi,
            "voyage" => EmbeddingProviderKind::Voyage,
            "openai-compatible" => EmbeddingProviderKind::OpenAiCompatible,
            other => return Err(ConfigError::InvalidProvider(other.into())),
        };

        let (default_base_url, default_model, default_dims) = match provider {
            EmbeddingProviderKind::OpenAi => {
                ("https://api.openai.com", "text-embedding-3-small", 1536)
            }
            EmbeddingProviderKind::Voyage => ("https://api.voyageai.com", "voyage-3", 1024),
            EmbeddingProviderKind::OpenAiCompatible => ("http://localhost:1234", "default", 1536),
        };

        let database_url = env::var("DATABASE_URL").ok();
        let storage_backend = Self::resolve_storage_backend(
            env::var("STORAGE_BACKEND").ok().as_deref(),
            database_url.is_some(),
        )?;
        let graph_settings = Self::parse_graph_settings();

        if storage_backend == StorageBackendKind::Postgres && database_url.is_none() {
            return Err(ConfigError::MissingDatabaseUrlForPostgres);
        }

        let chunking_strategy = env::var("CHUNKING_STRATEGY")
            .map(|s| ChunkingStrategy::from_str_loose(&s))
            .unwrap_or(ChunkingStrategy::Fixed);

        let chunking_semantic_threshold = env::var("CHUNKING_SEMANTIC_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.25);

        let pool_max_connections = env::var("POOL_MAX_CONNECTIONS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_POOL_MAX_CONNECTIONS);
        let pool_acquire_timeout_secs = env::var("POOL_ACQUIRE_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_POOL_ACQUIRE_TIMEOUT_SECS);

        Ok(Config {
            transport,
            host: env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into()),
            port: env::var("PORT").unwrap_or_else(|_| "3000".into()).parse()?,
            storage_backend,
            database_url,
            sqlite_path: env::var("SQLITE_PATH").unwrap_or_else(|_| "./wend-rag.db".into()),
            embedding_provider: provider,
            embedding_api_key: env::var("EMBEDDING_API_KEY").unwrap_or_default(),
            embedding_base_url: env::var("EMBEDDING_BASE_URL")
                .unwrap_or_else(|_| default_base_url.into()),
            embedding_model: env::var("EMBEDDING_MODEL").unwrap_or_else(|_| default_model.into()),
            embedding_dimensions: env::var("EMBEDDING_DIMENSIONS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(default_dims),
            entity_extraction_enabled: Self::parse_env_bool("ENTITY_EXTRACTION_ENABLED", false),
            entity_extraction_base_url: env::var("ENTITY_EXTRACTION_LLM_URL")
                .or_else(|_| env::var("EMBEDDING_BASE_URL"))
                .unwrap_or_else(|_| default_base_url.into()),
            entity_extraction_model: env::var("ENTITY_EXTRACTION_LLM_MODEL")
                .unwrap_or_else(|_| "gpt-4.1-mini".into()),
            entity_extraction_api_key: env::var("ENTITY_EXTRACTION_API_KEY")
                .or_else(|_| env::var("EMBEDDING_API_KEY"))
                .unwrap_or_default(),
            graph_settings,
            chunking_strategy,
            chunking_semantic_threshold,
            pool: PoolConfig {
                max_connections: pool_max_connections,
                acquire_timeout: Duration::from_secs(pool_acquire_timeout_secs),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, ConfigError, StorageBackendKind, TransportMode};
    use crate::entity::GraphSettings;

    /**
     * Verifies that an explicit SQLite backend choice wins even when a
     * PostgreSQL URL is available.
     */
    #[test]
    fn explicit_sqlite_backend_wins() {
        let backend = Config::resolve_storage_backend(Some("sqlite"), true).unwrap();
        assert_eq!(backend, StorageBackendKind::Sqlite);
    }

    /**
     * Verifies that explicit PostgreSQL mode fails fast when the required
     * database URL is missing from the environment.
     */
    #[test]
    fn postgres_without_database_url_is_rejected() {
        let backend = Config::resolve_storage_backend(Some("postgres"), false).unwrap();
        assert_eq!(backend, StorageBackendKind::Postgres);

        let error = if backend == StorageBackendKind::Postgres {
            Some(ConfigError::MissingDatabaseUrlForPostgres)
        } else {
            None
        };

        assert!(matches!(
            error,
            Some(ConfigError::MissingDatabaseUrlForPostgres)
        ));
    }

    /**
     * Verifies that SQLite becomes the default backend when no PostgreSQL URL
     * is configured.
     */
    #[test]
    fn sqlite_is_default_without_database_url() {
        let backend = Config::resolve_storage_backend(None, false).unwrap();
        assert_eq!(backend, StorageBackendKind::Sqlite);
    }

    /**
     * Verifies that the stdio transport flag overrides the environment value.
     */
    #[test]
    fn stdio_flag_overrides_transport_env() {
        let transport =
            Config::resolve_transport(&["wend-rag".into(), "--stdio".into()], Some("http"));
        assert_eq!(transport, TransportMode::Stdio);
    }

    /**
     * Verifies that graph settings clamp their traversal depth into the
     * supported range while preserving the explicit enable flag.
     */
    #[test]
    fn graph_settings_are_clamped() {
        let settings = GraphSettings::new(true, 9);
        assert!(settings.enabled);
        assert_eq!(settings.traversal_depth, 3);
    }
}
