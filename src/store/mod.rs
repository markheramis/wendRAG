use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use sqlx::ConnectOptions;
use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use uuid::Uuid;

use crate::config::{Config, PoolConfig, StorageBackendKind};
use crate::entity::DocumentEntityGraph;
use crate::retrieve::ScoredChunk;

pub mod models;
pub mod postgres;
pub mod sqlite;

pub use postgres::PostgresBackend;
pub use sqlite::SqliteBackend;

/** PostgreSQL migration set used when the server runs against pgvector. */
pub static POSTGRES_MIGRATOR: Migrator = sqlx::migrate!("./migrations/postgres");

/** SQLite migration set used for the local single-file backend. */
pub static SQLITE_MIGRATOR: Migrator = sqlx::migrate!("./migrations/sqlite");

#[derive(Debug, Clone)]
pub struct ChunkInsert {
    pub content: String,
    pub chunk_index: i32,
    pub section_title: Option<String>,
    pub embedding: Vec<f32>,
}

#[derive(Debug, Clone)]
pub struct DocumentUpsert {
    pub file_path: String,
    pub file_name: String,
    pub file_type: String,
    pub content_hash: String,
    pub project: Option<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SearchFilters {
    pub project: Option<String>,
    pub file_types: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreInitError {
    #[error("DATABASE_URL is required when the PostgreSQL backend is selected")]
    MissingDatabaseUrl,
    #[error("storage initialization failed: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("failed to prepare SQLite storage: {0}")]
    Io(#[from] std::io::Error),
}

#[async_trait]
pub trait StorageBackend: Send + Sync {
    /**
     * Loads the lightweight document projection used by the ingest pipeline to
     * detect unchanged content and create/update transitions.
     */
    async fn get_document_by_path(
        &self,
        file_path: &str,
    ) -> Result<Option<models::Document>, sqlx::Error>;

    /**
     * Inserts or updates a document row and returns its stable identifier.
     */
    async fn upsert_document(&self, input: &DocumentUpsert) -> Result<Uuid, sqlx::Error>;

    /**
     * Replaces every chunk belonging to a document in a single backend-local
     * transaction so sparse indexes remain consistent.
     */
    async fn replace_document_chunks(
        &self,
        document_id: Uuid,
        chunks: &[ChunkInsert],
    ) -> Result<(), sqlx::Error>;

    /**
     * Persists the optional entity graph derived from a document's chunks after
     * chunk replacement has completed. Backends without graph support may ignore
     * the payload.
     */
    async fn replace_document_entity_graph(
        &self,
        _document_id: Uuid,
        _graph: &DocumentEntityGraph,
    ) -> Result<(), sqlx::Error> {
        Ok(())
    }

    /**
     * Executes dense vector retrieval for the supplied embedding query.
     */
    async fn search_dense(
        &self,
        query_embedding: &[f32],
        top_k: i64,
        filters: &SearchFilters,
    ) -> Result<Vec<ScoredChunk>, sqlx::Error>;

    /**
     * Executes backend-native sparse retrieval and returns scored chunks sorted
     * from best to worst.
     */
    async fn search_sparse(
        &self,
        query: &str,
        top_k: i64,
        filters: &SearchFilters,
    ) -> Result<Vec<ScoredChunk>, sqlx::Error>;

    /**
     * Executes a graph-expansion retrieval branch seeded by already-ranked
     * chunks. Backends without graph support return an empty list.
     */
    async fn search_graph(
        &self,
        _seed_chunk_ids: &[Uuid],
        _top_k: i64,
        _filters: &SearchFilters,
        _traversal_depth: u8,
    ) -> Result<Vec<ScoredChunk>, sqlx::Error> {
        Ok(Vec::new())
    }

    /**
     * Returns the total count of documents and chunks for the status resource.
     * Derived from list_documents so no new backend implementation is required.
     */
    async fn count_documents_and_chunks(&self) -> Result<(u64, u64), sqlx::Error> {
        let docs = self.list_documents(None, None).await?;
        let doc_count = docs.len() as u64;
        let chunk_count: u64 = docs.iter().map(|d| d.chunk_count as u64).sum();
        Ok((doc_count, chunk_count))
    }

    /**
     * Lists indexed documents and their chunk counts with optional filters.
     */
    async fn list_documents(
        &self,
        project: Option<&str>,
        file_type: Option<&str>,
    ) -> Result<Vec<models::DocumentWithChunkCount>, sqlx::Error>;

    /**
     * Loads every stored chunk for a document path in ascending chunk order so
     * higher layers can reconstruct full-document context responses.
     */
    async fn get_document_chunks(
        &self,
        file_path: &str,
    ) -> Result<Vec<models::DocumentChunk>, sqlx::Error>;

    /**
     * Deletes a document selected by path or identifier and reports the removed
     * chunk count when a match exists.
     */
    async fn delete_document(
        &self,
        file_path: Option<&str>,
        document_id: Option<Uuid>,
    ) -> Result<Option<(String, i64)>, sqlx::Error>;
}

/**
 * Initializes the selected storage backend, runs its migrations, and returns a
 * trait object shared by the ingest, retrieval, and MCP layers.
 */
pub async fn initialize_backend(cfg: &Config) -> Result<Arc<dyn StorageBackend>, StoreInitError> {
    match cfg.storage_backend {
        StorageBackendKind::Postgres => {
            let database_url = cfg
                .database_url
                .as_deref()
                .ok_or(StoreInitError::MissingDatabaseUrl)?;
            let backend = PostgresBackend::connect(database_url, &cfg.pool).await?;
            Ok(Arc::new(backend))
        }
        StorageBackendKind::Sqlite => {
            let backend = SqliteBackend::connect(&cfg.sqlite_path, &cfg.pool).await?;
            Ok(Arc::new(backend))
        }
    }
}

/**
 * Builds the SQLx SQLite connection options used by the local backend and
 * creates parent directories for on-disk databases when needed.
 */
pub(crate) fn sqlite_connect_options(
    sqlite_path: &str,
) -> Result<SqliteConnectOptions, StoreInitError> {
    let mut options = SqliteConnectOptions::new()
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .create_if_missing(true);

    if sqlite_path == ":memory:" {
        options = options.in_memory(true);
    } else {
        let path = Path::new(sqlite_path);
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        options = options.filename(path);
    }

    Ok(options.disable_statement_logging())
}

/**
 * Creates the SQLite pool used by the local backend. Pool size and acquire
 * timeout are driven by the caller-provided [`PoolConfig`].
 */
pub(crate) async fn connect_sqlite_pool(
    sqlite_path: &str,
    pool_cfg: &PoolConfig,
) -> Result<sqlx::SqlitePool, StoreInitError> {
    let options = sqlite_connect_options(sqlite_path)?;
    let pool = SqlitePoolOptions::new()
        .max_connections(pool_cfg.max_connections)
        .acquire_timeout(pool_cfg.acquire_timeout)
        .connect_with(options)
        .await?;
    Ok(pool)
}
