use std::env;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::response::IntoResponse;
use axum::routing::get;
use wend_rag::config::ChunkingStrategy;
use wend_rag::embed::EmbeddingProvider;
use wend_rag::embed::provider::EmbeddingError;
use wend_rag::entity::{
    ChunkEntityExtraction, EntityExtractionError, EntityExtractionInput, EntityExtractor,
    ExtractedEntity, ExtractedRelationship, GraphSettings,
};
use wend_rag::ingest::pipeline::{
    self, ContentIngestRequest, DirectoryIngestRequest, IngestOptions, IngestStatus,
};
use wend_rag::retrieve::{dense, fusion, hybrid, sparse};
use wend_rag::store::{PostgresBackend, SearchFilters, SqliteBackend, StorageBackend};
use sqlx::postgres::PgPoolOptions;
use tempfile::TempDir;
use url::Url;
use uuid::Uuid;

type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct BackendHarness {
    name: &'static str,
    project: String,
    storage: Arc<dyn StorageBackend>,
    _temp_dir: Option<TempDir>,
    postgres_cleanup: Option<PostgresCleanup>,
}

struct PostgresCleanup {
    admin_database_url: String,
    database_name: String,
}

struct FakeEmbedder;
struct FakeExtractor;
struct UrlTestServer {
    base_url: String,
    handle: tokio::task::JoinHandle<()>,
}

#[derive(Clone)]
struct UrlTestState {
    robots_txt: String,
    article_html: String,
}

#[async_trait]
impl EmbeddingProvider for FakeEmbedder {
    /**
     * Produces deterministic 1024-dimensional embeddings so the tests can
     * validate both backends without calling a real embeddings API.
     */
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        Ok(texts.iter().map(|text| embed_text(text)).collect())
    }
}

#[async_trait]
impl EntityExtractor for FakeExtractor {
    /**
     * Produces deterministic entity and relationship payloads so the graph
     * retrieval tests do not depend on a live LLM endpoint.
     */
    async fn extract(
        &self,
        input: EntityExtractionInput<'_>,
    ) -> Result<ChunkEntityExtraction, EntityExtractionError> {
        let normalized = input.content.to_lowercase();
        let mut entities: Vec<ExtractedEntity> = Vec::new();
        let mut relationships: Vec<ExtractedRelationship> = Vec::new();

        if normalized.contains("atlas") {
            entities.push(ExtractedEntity {
                name: "Atlas".to_string(),
                entity_type: "service".to_string(),
                description: Some("Retrieval coordinator".to_string()),
            });
        }

        if normalized.contains("beacon") {
            entities.push(ExtractedEntity {
                name: "Beacon".to_string(),
                entity_type: "service".to_string(),
                description: Some("Cache invalidation service".to_string()),
            });
        }

        if normalized.contains("depends on beacon") {
            relationships.push(ExtractedRelationship {
                source_name: "Atlas".to_string(),
                source_type: "service".to_string(),
                target_name: "Beacon".to_string(),
                target_type: "service".to_string(),
                relationship_type: "depends_on".to_string(),
                description: Some("Atlas depends on Beacon".to_string()),
                weight: 1.0,
            });
        }

        Ok(ChunkEntityExtraction {
            chunk_index: input.chunk_index,
            entities,
            relationships,
        })
    }
}

/**
 * Verifies that both backends bootstrap successfully and start from an empty
 * document set for a fresh test workspace.
 */
#[tokio::test]
async fn backends_bootstrap_empty_store() -> TestResult<()> {
    for harness in available_backends().await? {
        let docs = harness
            .storage
            .list_documents(Some(&harness.project), None)
            .await?;
        assert!(
            docs.is_empty(),
            "{} backend should start empty for a fresh project",
            harness.name
        );
        harness.cleanup().await?;
    }

    Ok(())
}

/**
 * Verifies ingest, re-ingest, list, dense search, sparse search, hybrid fusion,
 * and delete semantics against every available backend.
 */
#[tokio::test]
async fn backends_match_ingest_and_search_behaviour() -> TestResult<()> {
    let embedder: Arc<dyn EmbeddingProvider> = Arc::new(FakeEmbedder);

    for harness in available_backends().await? {
        let alpha_path = format!("docs/{}/alpha-guide.md", harness.project);
        let beta_path = format!("docs/{}/beta-manual.md", harness.project);
        let alpha_tags = vec!["alpha".to_string()];
        let beta_tags = vec!["beta".to_string()];

        let alpha = pipeline::ingest_content(
            &harness.storage,
            &embedder,
            ContentIngestRequest {
                file_path: &alpha_path,
                file_name: "alpha-guide.md",
                file_type: "markdown",
                text: "# Alpha Guide\n\nAlpha orchard notes and local backend design details.",
            },
            &IngestOptions::new(
                Some(&harness.project),
                &alpha_tags,
                None,
                ChunkingStrategy::Fixed,
                0.25,
            ),
        )
        .await?;
        assert!(
            matches!(alpha.status, IngestStatus::Created),
            "{} backend should create the first alpha document",
            harness.name
        );
        assert!(
            alpha.chunk_count > 0,
            "{} backend should store at least one chunk for alpha",
            harness.name
        );

        let alpha_reingest = pipeline::ingest_content(
            &harness.storage,
            &embedder,
            ContentIngestRequest {
                file_path: &alpha_path,
                file_name: "alpha-guide.md",
                file_type: "markdown",
                text: "# Alpha Guide\n\nAlpha orchard notes and local backend design details.",
            },
            &IngestOptions::new(
                Some(&harness.project),
                &alpha_tags,
                None,
                ChunkingStrategy::Fixed,
                0.25,
            ),
        )
        .await?;
        assert!(
            matches!(alpha_reingest.status, IngestStatus::Unchanged),
            "{} backend should skip unchanged alpha content",
            harness.name
        );

        let beta = pipeline::ingest_content(
            &harness.storage,
            &embedder,
            ContentIngestRequest {
                file_path: &beta_path,
                file_name: "beta-manual.md",
                file_type: "markdown",
                text: "# Beta Manual\n\nBanana retrieval notes and beta release guidance.",
            },
            &IngestOptions::new(
                Some(&harness.project),
                &beta_tags,
                None,
                ChunkingStrategy::Fixed,
                0.25,
            ),
        )
        .await?;
        assert!(
            matches!(beta.status, IngestStatus::Created),
            "{} backend should create the beta document",
            harness.name
        );

        let listed_docs = harness
            .storage
            .list_documents(Some(&harness.project), Some("markdown"))
            .await?;
        assert_eq!(
            listed_docs.len(),
            2,
            "{} backend should list both markdown documents",
            harness.name
        );

        let filters = SearchFilters {
            project: Some(harness.project.clone()),
            file_types: Some(vec!["markdown".to_string()]),
            tags: None,
        };

        let dense_results =
            dense::search(&harness.storage, &embed_text("alpha query"), 5, &filters).await?;
        assert_eq!(
            dense_results.first().map(|chunk| chunk.file_name.as_str()),
            Some("alpha-guide.md"),
            "{} backend should rank alpha first for dense search",
            harness.name
        );

        let sparse_keyword_results =
            sparse::search(&harness.storage, "banana", 5, &filters).await?;
        assert!(
            sparse_keyword_results
                .iter()
                .any(|chunk| chunk.file_name == "beta-manual.md"),
            "{} backend should find beta via sparse keyword search",
            harness.name
        );

        let sparse_trigram_results =
            sparse::search(&harness.storage, "alpha-guide", 5, &filters).await?;
        assert!(
            sparse_trigram_results
                .iter()
                .any(|chunk| chunk.file_name == "alpha-guide.md"),
            "{} backend should find alpha via trigram/path search",
            harness.name
        );

        let hybrid_results = fusion::reciprocal_rank_fusion(
            &[
                dense_results.clone(),
                sparse::search(&harness.storage, "alpha", 5, &filters).await?,
            ],
            5,
        );
        assert_eq!(
            hybrid_results.first().map(|chunk| chunk.file_name.as_str()),
            Some("alpha-guide.md"),
            "{} backend should rank alpha first after hybrid fusion",
            harness.name
        );

        let deleted_alpha = harness
            .storage
            .delete_document(Some(&alpha_path), None)
            .await?;
        assert!(
            deleted_alpha.is_some(),
            "{} backend should delete alpha by file path",
            harness.name
        );

        let deleted_beta = harness
            .storage
            .delete_document(None, Some(beta.document_id))
            .await?;
        assert!(
            deleted_beta.is_some(),
            "{} backend should delete beta by document id",
            harness.name
        );

        let final_docs = harness
            .storage
            .list_documents(Some(&harness.project), None)
            .await?;
        assert!(
            final_docs.is_empty(),
            "{} backend should be empty after both deletes",
            harness.name
        );
        harness.cleanup().await?;
    }

    Ok(())
}

/**
 * Verifies that graph-aware hybrid search enriches results through entity
 * relationships on every available backend.
 */
#[tokio::test]
async fn graph_retrieval_works_on_all_backends() -> TestResult<()> {
    let embedder: Arc<dyn EmbeddingProvider> = Arc::new(FakeEmbedder);
    let extractor: Arc<dyn EntityExtractor> = Arc::new(FakeExtractor);

    for harness in available_backends().await? {
        let atlas_path = format!("docs/{}/atlas-overview.md", harness.project);
        let beacon_path = format!("docs/{}/beacon-cache.md", harness.project);
        let no_tags: Vec<String> = Vec::new();

        pipeline::ingest_content(
            &harness.storage,
            &embedder,
            ContentIngestRequest {
                file_path: &atlas_path,
                file_name: "atlas-overview.md",
                file_type: "markdown",
                text: "# Atlas Overview\n\nAtlas coordinates retrieval and depends on Beacon for cache invalidation.",
            },
            &IngestOptions::new(
                Some(&harness.project),
                &no_tags,
                Some(&extractor),
                ChunkingStrategy::Fixed,
                0.25,
            ),
        )
        .await?;
        pipeline::ingest_content(
            &harness.storage,
            &embedder,
            ContentIngestRequest {
                file_path: &beacon_path,
                file_name: "beacon-cache.md",
                file_type: "markdown",
                text: "# Beacon Cache\n\nBeacon handles cache invalidation for distributed search workers.",
            },
            &IngestOptions::new(
                Some(&harness.project),
                &no_tags,
                Some(&extractor),
                ChunkingStrategy::Fixed,
                0.25,
            ),
        )
        .await?;

        let filters = SearchFilters {
            project: Some(harness.project.clone()),
            file_types: Some(vec!["markdown".to_string()]),
            tags: None,
        };

        let baseline_results = hybrid::search(
            &harness.storage,
            &embedder,
            "atlas dependency",
            5,
            &filters,
            GraphSettings::new(false, 2),
        )
        .await?;
        let seed_chunk_ids = baseline_results
            .iter()
            .take(1)
            .map(|chunk| chunk.chunk_id)
            .collect::<Vec<_>>();
        let graph_branch = harness
            .storage
            .search_graph(&seed_chunk_ids, 5, &filters, 2)
            .await?;
        let graph_results = hybrid::search(
            &harness.storage,
            &embedder,
            "atlas dependency",
            5,
            &filters,
            GraphSettings::new(true, 2),
        )
        .await?;

        assert!(
            graph_branch
                .iter()
                .any(|chunk| chunk.file_name == "beacon-cache.md"),
            "{} backend graph branch should surface beacon through entity expansion",
            harness.name,
        );
        assert!(
            graph_results
                .iter()
                .any(|chunk| chunk.file_name == "beacon-cache.md"),
            "{} backend hybrid search should include beacon when graph retrieval is enabled",
            harness.name,
        );

        harness.cleanup().await?;
    }

    Ok(())
}

/**
 * Verifies that every backend can return a document's stored chunks in stable
 * chunk order so the MCP layer can reconstruct full-document context.
 */
#[tokio::test]
async fn backends_return_ordered_document_chunks_for_full_context() -> TestResult<()> {
    let embedder: Arc<dyn EmbeddingProvider> = Arc::new(FakeEmbedder);
    let no_tags: Vec<String> = Vec::new();

    for harness in available_backends().await? {
        let file_path = format!("docs/{}/full-context.txt", harness.project);
        let long_text = format!(
            "{}{}{}",
            "alpha ".repeat(170),
            "boundary overlap marker 0123456789 abcdefghijklmnopqrstuvwxyz ".repeat(4),
            "tail ".repeat(120),
        );
        let output = pipeline::ingest_content(
            &harness.storage,
            &embedder,
            ContentIngestRequest {
                file_path: &file_path,
                file_name: "full-context.txt",
                file_type: "text",
                text: &long_text,
            },
            &IngestOptions::new(
                Some(&harness.project),
                &no_tags,
                None,
                ChunkingStrategy::Fixed,
                0.25,
            ),
        )
        .await?;

        assert!(
            output.chunk_count > 1,
            "{} backend should create multiple chunks for the long document",
            harness.name
        );

        let chunks = harness.storage.get_document_chunks(&file_path).await?;
        assert_eq!(
            chunks.len(),
            output.chunk_count,
            "{} backend should return every stored chunk for the document",
            harness.name
        );
        assert!(
            chunks
                .windows(2)
                .all(|pair| pair[0].chunk_index < pair[1].chunk_index),
            "{} backend should return chunks in ascending chunk_index order",
            harness.name
        );

        harness.cleanup().await?;
    }

    Ok(())
}

/**
 * Verifies that URL ingestion converts readable HTML into stored `url`
 * documents and makes the extracted text searchable on every backend.
 */
#[tokio::test]
async fn url_ingestion_works_on_all_backends() -> TestResult<()> {
    let embedder: Arc<dyn EmbeddingProvider> = Arc::new(FakeEmbedder);
    let no_tags: Vec<String> = Vec::new();
    let server = UrlTestServer::start(
        "User-agent: *\nAllow: /\n",
        r#"<!doctype html>
        <html>
          <body>
            <nav>Navigation that should be stripped</nav>
            <article>
              <h1>Retrieval Quality Phase Two</h1>
              <p>URL ingestion should preserve readable article content.</p>
            </article>
          </body>
        </html>"#,
    )
    .await?;
    let article_url = server.article_url();

    for harness in available_backends().await? {
        let output = pipeline::ingest_path(
            &harness.storage,
            &embedder,
            None,
            &article_url,
            Some(&harness.project),
            &no_tags,
            ChunkingStrategy::Fixed,
            0.25,
        )
        .await?;

        assert_eq!(
            output.added, 1,
            "{} backend should ingest the URL document",
            harness.name
        );
        assert_eq!(
            output.failed, 0,
            "{} backend should not fail URL ingestion",
            harness.name
        );

        let listed_docs = harness
            .storage
            .list_documents(Some(&harness.project), Some("url"))
            .await?;
        assert_eq!(
            listed_docs.len(),
            1,
            "{} backend should list the ingested URL document",
            harness.name
        );
        assert_eq!(
            listed_docs[0].file_path, article_url,
            "{} backend should preserve the original URL as file_path",
            harness.name
        );
        assert_eq!(
            listed_docs[0].file_type, "url",
            "{} backend should store URL documents with file_type=url",
            harness.name
        );

        let filters = SearchFilters {
            project: Some(harness.project.clone()),
            file_types: Some(vec!["url".to_string()]),
            tags: None,
        };
        let sparse_results =
            sparse::search(&harness.storage, "readable article", 5, &filters).await?;
        assert!(
            sparse_results
                .iter()
                .any(|chunk| chunk.file_path == article_url),
            "{} backend should search extracted URL content",
            harness.name
        );

        harness.cleanup().await?;
    }

    Ok(())
}

/**
 * Verifies that URL ingestion refuses pages blocked by robots.txt and leaves
 * both backends unchanged when crawling is disallowed.
 */
#[tokio::test]
async fn url_ingestion_respects_robots_txt() -> TestResult<()> {
    let embedder: Arc<dyn EmbeddingProvider> = Arc::new(FakeEmbedder);
    let no_tags: Vec<String> = Vec::new();
    let server = UrlTestServer::start(
        "User-agent: *\nDisallow: /article\n",
        r#"<html><body><article><p>Blocked content</p></article></body></html>"#,
    )
    .await?;
    let article_url = server.article_url();

    for harness in available_backends().await? {
        let error = pipeline::ingest_path(
            &harness.storage,
            &embedder,
            None,
            &article_url,
            Some(&harness.project),
            &no_tags,
            ChunkingStrategy::Fixed,
            0.25,
        )
        .await
        .expect_err("robots.txt should block URL ingestion");

        assert!(
            error.to_string().contains("robots.txt disallows"),
            "{} backend should surface the robots.txt denial",
            harness.name
        );

        let listed_docs = harness
            .storage
            .list_documents(Some(&harness.project), None)
            .await?;
        assert!(
            listed_docs.is_empty(),
            "{} backend should remain empty after a blocked URL ingestion",
            harness.name
        );

        harness.cleanup().await?;
    }

    Ok(())
}

/**
 * Builds the backend list used by the parity tests. SQLite always runs, while
 * PostgreSQL joins the matrix when a test database URL is configured.
 */
async fn available_backends() -> TestResult<Vec<BackendHarness>> {
    let mut backends = vec![sqlite_harness().await?];

    if let Some(postgres) = postgres_harness().await? {
        backends.push(postgres);
    }

    Ok(backends)
}

/**
 * Starts an ephemeral HTTP server that serves deterministic robots.txt and
 * article responses for URL-ingestion integration tests.
 */
impl UrlTestServer {
    async fn start(robots_txt: &str, article_html: &str) -> TestResult<Self> {
        let state = Arc::new(UrlTestState {
            robots_txt: robots_txt.to_string(),
            article_html: article_html.to_string(),
        });
        let app = Router::new()
            .route("/robots.txt", get(serve_test_robots))
            .route("/article", get(serve_test_article))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("test HTTP server should stay alive during the test");
        });

        Ok(Self {
            base_url: format!("http://{address}"),
            handle,
        })
    }

    /**
     * Returns the fully-qualified article URL served by the test server.
     */
    fn article_url(&self) -> String {
        format!("{}/article", self.base_url)
    }
}

impl Drop for UrlTestServer {
    /**
     * Stops the ephemeral HTTP server once the surrounding test scope ends.
     */
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/**
 * Creates a fresh SQLite backend rooted in a temporary database file.
 */
async fn sqlite_harness() -> TestResult<BackendHarness> {
    let temp_dir = tempfile::tempdir()?;
    let sqlite_path = temp_dir.path().join("code-rag.db");
    let pool_cfg = wend_rag::config::PoolConfig::default();
    let storage = Arc::new(SqliteBackend::connect(sqlite_path.to_str().unwrap(), &pool_cfg).await?)
        as Arc<dyn StorageBackend>;

    Ok(BackendHarness {
        name: "sqlite",
        project: format!("sqlite-{}", Uuid::new_v4()),
        storage,
        _temp_dir: Some(temp_dir),
        postgres_cleanup: None,
    })
}

/**
 * Creates a PostgreSQL backend when the caller has provided a dedicated test
 * database URL. When no URL exists the PostgreSQL half of the matrix is skipped.
 */
async fn postgres_harness() -> TestResult<Option<BackendHarness>> {
    let admin_database_url = env::var("TEST_DATABASE_URL")
        .ok()
        .or_else(|| env::var("DATABASE_URL").ok());

    let Some(admin_database_url) = admin_database_url else {
        return Ok(None);
    };

    let database_name = format!("code_rag_test_{}", Uuid::new_v4().simple());
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_database_url)
        .await?;
    sqlx::query(&format!("CREATE DATABASE \"{database_name}\""))
        .execute(&admin_pool)
        .await?;

    let mut database_url = Url::parse(&admin_database_url)?;
    database_url.set_path(&format!("/{database_name}"));

    let pool_cfg = wend_rag::config::PoolConfig::default();
    let storage =
        Arc::new(PostgresBackend::connect(database_url.as_str(), &pool_cfg).await?) as Arc<dyn StorageBackend>;
    Ok(Some(BackendHarness {
        name: "postgres",
        project: format!("postgres-{}", Uuid::new_v4()),
        storage,
        _temp_dir: None,
        postgres_cleanup: Some(PostgresCleanup {
            admin_database_url,
            database_name,
        }),
    }))
}

/**
 * Serves the deterministic robots.txt payload for URL ingestion tests.
 */
async fn serve_test_robots(State(state): State<Arc<UrlTestState>>) -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "text/plain; charset=utf-8")],
        (*state).robots_txt.clone(),
    )
}

/**
 * Serves deterministic HTML content for URL ingestion tests.
 */
async fn serve_test_article(State(state): State<Arc<UrlTestState>>) -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "text/html; charset=utf-8")],
        (*state).article_html.clone(),
    )
}

/**
 * Produces a deterministic embedding by projecting known keywords into fixed
 * vector dimensions and leaving the rest at zero.
 */
fn embed_text(text: &str) -> Vec<f32> {
    let normalized = text.to_lowercase();
    let mut embedding = vec![0.0; 1024];

    if normalized.contains("alpha") {
        embedding[0] = 1.0;
    }
    if normalized.contains("beta") || normalized.contains("banana") {
        embedding[1] = 1.0;
    }
    if normalized.contains("orchard") {
        embedding[2] = 0.5;
    }
    if normalized.contains("release") {
        embedding[3] = 0.5;
    }
    if normalized.contains("atlas") {
        embedding[4] = 1.0;
    }
    if normalized.contains("beacon") {
        embedding[5] = 1.0;
    }
    if normalized.contains("dependency") || normalized.contains("depends") {
        embedding[6] = 0.5;
    }
    if normalized.contains("cache") {
        embedding[7] = 0.5;
    }
    if normalized.contains("retrieval") {
        embedding[8] = 0.5;
    }

    if embedding.iter().all(|value| *value == 0.0) {
        embedding[1023] = 1.0;
    }

    embedding
}

// ─── Incremental sync refinement tests ───────────────────────────────────────

/**
 * Verifies that directory ingestion reports accurate added, updated, and
 * unchanged counters across first-run and re-run scenarios, and that
 * `delete_removed` removes orphaned documents.
 */
async fn run_incremental_sync_test(harness: &BackendHarness) -> TestResult<()> {
    let embedder: Arc<dyn EmbeddingProvider> = Arc::new(FakeEmbedder);
    let temp_dir = tempfile::tempdir()?;
    let dir_path = temp_dir.path();

    // Write two markdown files
    std::fs::write(dir_path.join("one.md"), "# Alpha\nFirst document.")?;
    std::fs::write(dir_path.join("two.md"), "# Beta\nSecond document.")?;

    let options = IngestOptions::new(
        Some(&harness.project),
        &[],
        None,
        ChunkingStrategy::Fixed,
        0.25,
    );

    // ── First ingest: both files should be "created" ─────────────────────
    let result = pipeline::ingest_directory(
        &harness.storage,
        &embedder,
        DirectoryIngestRequest {
            directory_path: dir_path.to_str().unwrap(),
            recursive: false,
            glob_pattern: None,
            delete_removed: false,
        },
        &options,
    )
    .await?;

    assert_eq!(result.added, 2, "{}: first ingest should add 2 files", harness.name);
    assert_eq!(result.updated, 0, "{}: first ingest should update 0", harness.name);
    assert_eq!(result.unchanged, 0, "{}: first ingest should have 0 unchanged", harness.name);
    assert_eq!(result.deleted, 0, "{}: first ingest should delete 0", harness.name);
    assert_eq!(result.failed, 0, "{}: first ingest should have 0 failures", harness.name);

    // ── Re-ingest without changes: both should be "unchanged" ────────────
    let result = pipeline::ingest_directory(
        &harness.storage,
        &embedder,
        DirectoryIngestRequest {
            directory_path: dir_path.to_str().unwrap(),
            recursive: false,
            glob_pattern: None,
            delete_removed: false,
        },
        &options,
    )
    .await?;

    assert_eq!(result.added, 0, "{}: re-ingest should add 0", harness.name);
    assert_eq!(result.updated, 0, "{}: re-ingest should update 0", harness.name);
    assert_eq!(result.unchanged, 2, "{}: re-ingest should have 2 unchanged", harness.name);

    // ── Modify one file and re-ingest: 1 updated, 1 unchanged ───────────
    std::fs::write(dir_path.join("one.md"), "# Alpha\nFirst document, now modified.")?;

    let result = pipeline::ingest_directory(
        &harness.storage,
        &embedder,
        DirectoryIngestRequest {
            directory_path: dir_path.to_str().unwrap(),
            recursive: false,
            glob_pattern: None,
            delete_removed: false,
        },
        &options,
    )
    .await?;

    assert_eq!(result.added, 0, "{}: modified ingest should add 0", harness.name);
    assert_eq!(result.updated, 1, "{}: modified ingest should update 1", harness.name);
    assert_eq!(result.unchanged, 1, "{}: modified ingest should have 1 unchanged", harness.name);

    // ── Remove one file and re-ingest with delete_removed=false ──────────
    std::fs::remove_file(dir_path.join("two.md"))?;

    let result = pipeline::ingest_directory(
        &harness.storage,
        &embedder,
        DirectoryIngestRequest {
            directory_path: dir_path.to_str().unwrap(),
            recursive: false,
            glob_pattern: None,
            delete_removed: false,
        },
        &options,
    )
    .await?;

    assert_eq!(result.deleted, 0, "{}: delete_removed=false should not delete", harness.name);
    // two.md should still exist in storage
    let docs = harness.storage.list_documents(None, None).await?;
    let has_two = docs.iter().any(|d| d.file_path.contains("two.md"));
    assert!(has_two, "{}: two.md should still be in storage when delete_removed=false", harness.name);

    // ── Re-ingest with delete_removed=true: orphan should be removed ─────
    let result = pipeline::ingest_directory(
        &harness.storage,
        &embedder,
        DirectoryIngestRequest {
            directory_path: dir_path.to_str().unwrap(),
            recursive: false,
            glob_pattern: None,
            delete_removed: true,
        },
        &options,
    )
    .await?;

    assert_eq!(result.deleted, 1, "{}: delete_removed=true should delete 1 orphan", harness.name);
    assert_eq!(result.unchanged, 1, "{}: remaining file should be unchanged", harness.name);

    // Verify the orphan doc entry was added to the documents list
    let deleted_entry = result.documents.iter().find(|d| d.status == "deleted");
    assert!(deleted_entry.is_some(), "{}: should have a 'deleted' status entry", harness.name);
    assert!(
        deleted_entry.unwrap().file_path.contains("two.md"),
        "{}: deleted entry should reference two.md",
        harness.name,
    );

    // Verify storage no longer has two.md
    let docs = harness.storage.list_documents(None, None).await?;
    let has_two = docs.iter().any(|d| d.file_path.contains("two.md"));
    assert!(!has_two, "{}: two.md should be removed from storage after delete_removed=true", harness.name);

    Ok(())
}

#[tokio::test]
async fn incremental_sync_sqlite() -> TestResult<()> {
    let harness = sqlite_harness().await?;
    run_incremental_sync_test(&harness).await?;
    harness.cleanup().await
}

#[tokio::test]
async fn incremental_sync_postgres() -> TestResult<()> {
    let Some(harness) = postgres_harness().await? else {
        eprintln!("skipping postgres incremental sync test – no TEST_DATABASE_URL");
        return Ok(());
    };
    run_incremental_sync_test(&harness).await?;
    harness.cleanup().await
}

impl BackendHarness {
    /**
     * Tears down backend-specific temporary state after a test harness has
     * finished using its storage backend.
     */
    async fn cleanup(self) -> TestResult<()> {
        let cleanup = self.postgres_cleanup;
        drop(self.storage);
        drop(self._temp_dir);

        if let Some(cleanup) = cleanup {
            let admin_pool = PgPoolOptions::new()
                .max_connections(1)
                .connect(&cleanup.admin_database_url)
                .await?;
            sqlx::query(
                "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = $1 AND pid <> pg_backend_pid()",
            )
            .bind(&cleanup.database_name)
            .execute(&admin_pool)
            .await?;
            sqlx::query(&format!(
                "DROP DATABASE IF EXISTS \"{}\"",
                cleanup.database_name
            ))
            .execute(&admin_pool)
            .await?;
        }

        Ok(())
    }
}
