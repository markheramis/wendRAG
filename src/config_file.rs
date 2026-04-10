use std::path::PathBuf;

use serde::Deserialize;

/// Default system-wide config path on Linux.
#[cfg(target_os = "linux")]
const DEFAULT_CONFIG_PATH: &str = "/etc/wend-rag/config.yaml";

/**
 * Top-level YAML configuration file structure. Every field is optional so that
 * operators can specify only the values they care about; missing fields fall
 * through to environment variables and then compiled defaults.
 */
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct FileConfig {
    pub server: ServerSection,
    pub storage: StorageSection,
    pub embedding: EmbeddingSection,
    pub entity_extraction: EntityExtractionSection,
    pub graph: GraphSection,
    pub chunking: ChunkingSection,
    pub pool: PoolSection,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ServerSection {
    pub host: Option<String>,
    pub port: Option<u16>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct StorageSection {
    pub backend: Option<String>,
    pub database_url: Option<String>,
    pub sqlite_path: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct EmbeddingSection {
    pub provider: Option<String>,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub dimensions: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct EntityExtractionSection {
    pub enabled: Option<bool>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct GraphSection {
    pub enabled: Option<bool>,
    pub traversal_depth: Option<u8>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ChunkingSection {
    pub strategy: Option<String>,
    pub semantic_threshold: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct PoolSection {
    pub max_connections: Option<u32>,
    pub acquire_timeout_secs: Option<u64>,
}

impl FileConfig {
    /**
     * Resolves the config file path and loads it if present.
     *
     * Path resolution order:
     *   1. Explicit CLI `--config` value (`cli_path`)
     *   2. `WEND_RAG_CONFIG` environment variable
     *   3. `/etc/wend-rag/config.yaml` (Linux only)
     *
     * Returns `None` when no file is found at any candidate path.
     * Logs a warning and returns `None` on parse errors so the application
     * can still fall back to environment variables.
     */
    pub fn load(cli_path: Option<&str>) -> Option<Self> {
        let path = Self::resolve_path(cli_path)?;

        let contents = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(path = %path.display(), "config file not found, skipping");
                return None;
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "failed to read config file");
                return None;
            }
        };

        match serde_yml::from_str::<FileConfig>(&contents) {
            Ok(cfg) => {
                tracing::info!(path = %path.display(), "loaded config from file");
                Some(cfg)
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "failed to parse config file");
                None
            }
        }
    }

    /**
     * Determines the config file path from CLI flag, env var, or platform
     * default. Returns `None` when no candidate exists (e.g. non-Linux with
     * no explicit path).
     */
    fn resolve_path(cli_path: Option<&str>) -> Option<PathBuf> {
        if let Some(p) = cli_path {
            return Some(PathBuf::from(p));
        }

        if let Ok(p) = std::env::var("WEND_RAG_CONFIG") {
            if !p.is_empty() {
                return Some(PathBuf::from(p));
            }
        }

        #[cfg(target_os = "linux")]
        {
            let default = PathBuf::from(DEFAULT_CONFIG_PATH);
            if default.exists() {
                return Some(default);
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /**
     * Verifies that a fully-populated YAML string deserializes into a
     * `FileConfig` with all fields present.
     */
    #[test]
    fn parses_full_yaml_config() {
        let yaml = r#"
server:
  host: "127.0.0.1"
  port: 8080
storage:
  backend: "postgres"
  database_url: "postgres://u:p@localhost/db"
  sqlite_path: "./test.db"
embedding:
  provider: "voyage"
  api_key: "sk-test"
  base_url: "https://api.voyage.com"
  model: "voyage-3"
  dimensions: 1024
entity_extraction:
  enabled: true
  base_url: "http://localhost:11434"
  model: "llama3.2"
  api_key: "ext-key"
graph:
  enabled: true
  traversal_depth: 3
chunking:
  strategy: "semantic"
  semantic_threshold: 0.30
pool:
  max_connections: 10
  acquire_timeout_secs: 30
"#;
        let cfg: FileConfig = serde_yml::from_str(yaml).unwrap();

        assert_eq!(cfg.server.host.as_deref(), Some("127.0.0.1"));
        assert_eq!(cfg.server.port, Some(8080));
        assert_eq!(cfg.storage.backend.as_deref(), Some("postgres"));
        assert_eq!(
            cfg.storage.database_url.as_deref(),
            Some("postgres://u:p@localhost/db")
        );
        assert_eq!(cfg.embedding.provider.as_deref(), Some("voyage"));
        assert_eq!(cfg.embedding.dimensions, Some(1024));
        assert_eq!(cfg.entity_extraction.enabled, Some(true));
        assert_eq!(cfg.graph.enabled, Some(true));
        assert_eq!(cfg.graph.traversal_depth, Some(3));
        assert_eq!(cfg.chunking.strategy.as_deref(), Some("semantic"));
        assert_eq!(cfg.chunking.semantic_threshold, Some(0.30));
        assert_eq!(cfg.pool.max_connections, Some(10));
        assert_eq!(cfg.pool.acquire_timeout_secs, Some(30));
    }

    /**
     * Verifies that a partial YAML (only some sections) deserializes
     * successfully with all missing fields as `None`.
     */
    #[test]
    fn parses_partial_yaml_config() {
        let yaml = r#"
server:
  port: 9090
"#;
        let cfg: FileConfig = serde_yml::from_str(yaml).unwrap();

        assert_eq!(cfg.server.port, Some(9090));
        assert!(cfg.server.host.is_none());
        assert!(cfg.storage.backend.is_none());
        assert!(cfg.embedding.provider.is_none());
    }

    /**
     * Verifies that an empty YAML string produces a default `FileConfig`
     * with all fields set to `None`.
     */
    #[test]
    fn parses_empty_yaml() {
        let cfg: FileConfig = serde_yml::from_str("").unwrap();

        assert!(cfg.server.host.is_none());
        assert!(cfg.server.port.is_none());
        assert!(cfg.storage.backend.is_none());
    }

    /**
     * Verifies that a missing config file results in `None` without an error.
     */
    #[test]
    fn missing_file_returns_none() {
        let result = FileConfig::load(Some("/nonexistent/path/config.yaml"));
        assert!(result.is_none());
    }

    /**
     * Verifies that a temp file with valid YAML content loads successfully.
     */
    #[test]
    fn loads_from_explicit_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(
            &path,
            "server:\n  host: \"10.0.0.1\"\n  port: 5000\n",
        )
        .unwrap();

        let cfg = FileConfig::load(Some(path.to_str().unwrap())).unwrap();
        assert_eq!(cfg.server.host.as_deref(), Some("10.0.0.1"));
        assert_eq!(cfg.server.port, Some(5000));
    }
}
