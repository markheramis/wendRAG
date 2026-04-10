use std::env;
use std::time::Duration;

use crate::config_file::FileConfig;
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
 * backends. Parsed from `WEND_RAG_POOL_MAX_CONNECTIONS` and
 * `WEND_RAG_POOL_ACQUIRE_TIMEOUT_SECS` environment variables with safe
 * production defaults.
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
    #[error("WEND_RAG_DATABASE_URL is required when storage backend is postgres")]
    MissingDatabaseUrlForPostgres,
}

/**
 * Returns the YAML file value if present, otherwise falls back to the
 * environment variable. Returns `None` when neither source provides a value.
 *
 * Precedence: YAML (highest) > env var (lowest of the two).
 */
fn yaml_or_env(env_name: &str, yaml_value: Option<String>) -> Option<String> {
    if yaml_value.is_some() {
        return yaml_value;
    }
    env::var(env_name).ok()
}

/**
 * Parses a relaxed boolean from an optional string, accepting common truthy
 * values (`1`, `true`, `yes`, `on`) while treating everything else as false.
 */
fn parse_loose_bool(value: Option<&str>, default: bool) -> bool {
    match value {
        Some(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        None => default,
    }
}

impl Config {
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
     * Loads runtime configuration by merging layers in order of increasing
     * priority:
     *
     *   1. Compiled defaults (lowest)
     *   2. `WEND_RAG_*` environment variables
     *   3. `.env` file (loaded into the process environment by dotenvy)
     *   4. YAML config file values (highest)
     *
     * The `.env` file is loaded into the process env via `dotenvy::dotenv()`
     * before any `env::var` call, so layers 2 and 3 are read together from
     * `env::var`. The YAML layer (`file_config`) then overrides on top.
     */
    pub fn load(file_config: Option<&FileConfig>) -> Result<Self, ConfigError> {
        dotenvy::dotenv().ok();

        let empty = FileConfig::default();
        let fc = file_config.unwrap_or(&empty);

        let provider_str = yaml_or_env("WEND_RAG_EMBEDDING_PROVIDER", fc.embedding.provider.clone())
            .unwrap_or_else(|| "openai".into());
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

        let database_url =
            yaml_or_env("WEND_RAG_DATABASE_URL", fc.storage.database_url.clone());
        let storage_backend_str =
            yaml_or_env("WEND_RAG_STORAGE_BACKEND", fc.storage.backend.clone());
        let storage_backend = Self::resolve_storage_backend(
            storage_backend_str.as_deref(),
            database_url.is_some(),
        )?;

        if storage_backend == StorageBackendKind::Postgres && database_url.is_none() {
            return Err(ConfigError::MissingDatabaseUrlForPostgres);
        }

        let graph_enabled_str =
            yaml_or_env("WEND_RAG_GRAPH_RETRIEVAL_ENABLED", fc.graph.enabled.map(|b| b.to_string()));
        let graph_enabled = parse_loose_bool(graph_enabled_str.as_deref(), false);

        let graph_depth = fc
            .graph
            .traversal_depth
            .or_else(|| {
                env::var("WEND_RAG_GRAPH_TRAVERSAL_DEPTH")
                    .ok()
                    .and_then(|v| v.parse::<u8>().ok())
            })
            .unwrap_or(DEFAULT_GRAPH_TRAVERSAL_DEPTH);
        let graph_settings = GraphSettings::new(graph_enabled, graph_depth);

        let chunking_strategy_str =
            yaml_or_env("WEND_RAG_CHUNKING_STRATEGY", fc.chunking.strategy.clone());
        let chunking_strategy = chunking_strategy_str
            .map(|s| ChunkingStrategy::from_str_loose(&s))
            .unwrap_or(ChunkingStrategy::Fixed);

        let chunking_semantic_threshold = fc
            .chunking
            .semantic_threshold
            .or_else(|| {
                env::var("WEND_RAG_CHUNKING_SEMANTIC_THRESHOLD")
                    .ok()
                    .and_then(|v| v.parse().ok())
            })
            .unwrap_or(0.25);

        let host = yaml_or_env("WEND_RAG_HOST", fc.server.host.clone())
            .unwrap_or_else(|| "0.0.0.0".into());

        let port_str = yaml_or_env(
            "WEND_RAG_PORT",
            fc.server.port.map(|p| p.to_string()),
        )
        .unwrap_or_else(|| "3000".into());
        let port: u16 = port_str.parse()?;

        let sqlite_path = yaml_or_env("WEND_RAG_SQLITE_PATH", fc.storage.sqlite_path.clone())
            .unwrap_or_else(|| "./wend-rag.db".into());

        let embedding_api_key =
            yaml_or_env("WEND_RAG_EMBEDDING_API_KEY", fc.embedding.api_key.clone())
                .unwrap_or_default();

        let embedding_base_url =
            yaml_or_env("WEND_RAG_EMBEDDING_BASE_URL", fc.embedding.base_url.clone())
                .unwrap_or_else(|| default_base_url.into());

        let embedding_model =
            yaml_or_env("WEND_RAG_EMBEDDING_MODEL", fc.embedding.model.clone())
                .unwrap_or_else(|| default_model.into());

        let embedding_dimensions = fc
            .embedding
            .dimensions
            .or_else(|| {
                env::var("WEND_RAG_EMBEDDING_DIMENSIONS")
                    .ok()
                    .and_then(|v| v.parse().ok())
            })
            .unwrap_or(default_dims);

        let entity_extraction_enabled_str = yaml_or_env(
            "WEND_RAG_ENTITY_EXTRACTION_ENABLED",
            fc.entity_extraction.enabled.map(|b| b.to_string()),
        );
        let entity_extraction_enabled =
            parse_loose_bool(entity_extraction_enabled_str.as_deref(), false);

        let entity_extraction_base_url = yaml_or_env(
            "WEND_RAG_ENTITY_EXTRACTION_LLM_URL",
            fc.entity_extraction.base_url.clone(),
        )
        .or_else(|| yaml_or_env("WEND_RAG_EMBEDDING_BASE_URL", fc.embedding.base_url.clone()))
        .unwrap_or_else(|| default_base_url.into());

        let entity_extraction_model = yaml_or_env(
            "WEND_RAG_ENTITY_EXTRACTION_LLM_MODEL",
            fc.entity_extraction.model.clone(),
        )
        .unwrap_or_else(|| "gpt-4.1-mini".into());

        let entity_extraction_api_key = yaml_or_env(
            "WEND_RAG_ENTITY_EXTRACTION_API_KEY",
            fc.entity_extraction.api_key.clone(),
        )
        .or_else(|| yaml_or_env("WEND_RAG_EMBEDDING_API_KEY", fc.embedding.api_key.clone()))
        .unwrap_or_default();

        let pool_max_connections = fc
            .pool
            .max_connections
            .or_else(|| {
                env::var("WEND_RAG_POOL_MAX_CONNECTIONS")
                    .ok()
                    .and_then(|v| v.parse().ok())
            })
            .unwrap_or(DEFAULT_POOL_MAX_CONNECTIONS);

        let pool_acquire_timeout_secs = fc
            .pool
            .acquire_timeout_secs
            .or_else(|| {
                env::var("WEND_RAG_POOL_ACQUIRE_TIMEOUT_SECS")
                    .ok()
                    .and_then(|v| v.parse().ok())
            })
            .unwrap_or(DEFAULT_POOL_ACQUIRE_TIMEOUT_SECS);

        Ok(Config {
            host,
            port,
            storage_backend,
            database_url,
            sqlite_path,
            embedding_provider: provider,
            embedding_api_key,
            embedding_base_url,
            embedding_model,
            embedding_dimensions,
            entity_extraction_enabled,
            entity_extraction_base_url,
            entity_extraction_model,
            entity_extraction_api_key,
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
    use super::{Config, ConfigError, StorageBackendKind};
    use crate::config_file::FileConfig;
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
     * Verifies that graph settings clamp their traversal depth into the
     * supported range while preserving the explicit enable flag.
     */
    #[test]
    fn graph_settings_are_clamped() {
        let settings = GraphSettings::new(true, 9);
        assert!(settings.enabled);
        assert_eq!(settings.traversal_depth, 3);
    }

    /**
     * Verifies that YAML values take priority over environment variables.
     * Uses a FileConfig with an explicit port, while the env would default
     * to 3000.
     */
    #[test]
    fn yaml_overrides_env_values() {
        let yaml = r#"
server:
  port: 9999
storage:
  backend: "sqlite"
  sqlite_path: "/tmp/test.db"
embedding:
  provider: "openai"
"#;
        let fc: FileConfig = serde_yml::from_str(yaml).unwrap();
        let cfg = Config::load(Some(&fc)).unwrap();

        assert_eq!(cfg.port, 9999);
        assert_eq!(cfg.sqlite_path, "/tmp/test.db");
    }

    /**
     * Verifies that a YAML-only config (no env vars) produces a config that
     * reflects the YAML values with compiled defaults filling the gaps.
     */
    #[test]
    fn loads_from_yaml_with_compiled_defaults() {
        let yaml = r#"
storage:
  backend: "sqlite"
embedding:
  provider: "openai"
"#;
        let fc: FileConfig = serde_yml::from_str(yaml).unwrap();
        let cfg = Config::load(Some(&fc)).unwrap();
        assert_eq!(cfg.host, "0.0.0.0");
        assert_eq!(cfg.port, 3000);
        assert_eq!(cfg.storage_backend, StorageBackendKind::Sqlite);
    }
}
