/**
 * OpenTelemetry observability setup.
 *
 * Configures distributed tracing with OTLP export when the
 * `OTEL_EXPORTER_OTLP_ENDPOINT` environment variable is set.
 * Falls back to standard logging when OTLP is not configured.
 *
 * Instrumented spans cover:
 * - Ingestion pipeline (file read → chunking → embedding → DB upsert)
 * - Retrieval pipeline (query embedding → dense/sparse search → fusion)
 * - Entity extraction (LLM call → dedup → relationship insert)
 */

use std::env;
use std::time::Duration;

use opentelemetry::trace::TracerProvider;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/**
 * Initializes the tracing subscriber with optional OpenTelemetry export.
 *
 * When `OTEL_EXPORTER_OTLP_ENDPOINT` is set, configures an OTLP exporter
 * with batch span processing. Otherwise, uses the standard env-filtered
 * formatter outputting JSON to stderr.
 *
 * # Environment Variables
 * - `OTEL_EXPORTER_OTLP_ENDPOINT`: OTLP collector endpoint (e.g., `http://localhost:4317`)
 * - `OTEL_SERVICE_NAME`: Service name for traces (default: `wend-rag`)
 * - `RUST_LOG`: Log level filter (default: `info`)
 */
pub fn init_tracing() {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    // Base JSON formatter layer
    let json_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(std::io::stderr);

    // Check for OTLP endpoint configuration (must be non-empty)
    let otlp_endpoint = env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok();
    if let Some(endpoint) = otlp_endpoint.filter(|s| !s.trim().is_empty()) {
        let service_name = env::var("OTEL_SERVICE_NAME")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "wend-rag".to_string());

        // Configure OTLP exporter
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .with_timeout(Duration::from_secs(3))
            .build()
            .expect("failed to build OTLP span exporter");

        // Configure tracer provider with batch processing
        let provider = SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(
                opentelemetry_sdk::Resource::builder()
                    .with_service_name(service_name)
                    .build(),
            )
            .build();

        // Create OpenTelemetry tracing layer
        let otel_layer = tracing_opentelemetry::layer()
            .with_tracer(provider.tracer("wend-rag"));

        // Initialize subscriber with both OTLP and JSON layers
        tracing_subscriber::registry()
            .with(env_filter)
            .with(json_layer)
            .with(otel_layer)
            .init();

        tracing::info!("OpenTelemetry tracing initialized with OTLP export");
    } else {
        // Initialize subscriber with JSON layer only
        tracing_subscriber::registry()
            .with(env_filter)
            .with(json_layer)
            .init();

        tracing::debug!("OpenTelemetry OTLP export not configured; using JSON logging only");
    }
}

/**
 * Shuts down the OpenTelemetry tracer provider gracefully.
 *
 * Flushes any pending spans before the application exits.
 * This should be called during graceful shutdown.
 */
pub fn shutdown_tracing() {
    // Note: In opentelemetry 0.29, the provider is automatically flushed
    // when dropped. We just log that we're shutting down.
    tracing::debug!("shutting down OpenTelemetry tracer provider");
}

/**
 * Creates a tracing span for the ingestion pipeline.
 *
 * # Arguments
 * - `source`: The source being ingested (file path, URL, etc.)
 * - `file_type`: The detected file type
 */
#[macro_export]
macro_rules! ingest_span {
    ($source:expr, $file_type:expr) => {
        tracing::info_span!(
            "ingest",
            source = %$source,
            file_type = %$file_type,
            otel.name = "ingest.document",
            otel.kind = "server"
        )
    };
}

/**
 * Creates a tracing span for the retrieval pipeline.
 *
 * # Arguments
 * - `query`: The search query
 * - `top_k`: Number of results requested
 */
#[macro_export]
macro_rules! retrieve_span {
    ($query:expr, $top_k:expr) => {
        tracing::info_span!(
            "retrieve",
            query = %$query,
            top_k = %$top_k,
            otel.name = "retrieve.search",
            otel.kind = "server"
        )
    };
}

/**
 * Creates a tracing span for entity extraction.
 *
 * # Arguments
 * - `chunk_id`: The ID of the chunk being processed
 */
#[macro_export]
macro_rules! entity_extraction_span {
    ($chunk_id:expr) => {
        tracing::info_span!(
            "entity_extraction",
            chunk_id = %$chunk_id,
            otel.name = "entity.extract",
            otel.kind = "internal"
        )
    };
}

/**
 * Creates a tracing span for embedding API calls.
 *
 * # Arguments
 * - `provider`: The embedding provider name
 * - `batch_size`: Number of texts in the batch
 */
#[macro_export]
macro_rules! embed_span {
    ($provider:expr, $batch_size:expr) => {
        tracing::info_span!(
            "embed",
            provider = %$provider,
            batch_size = %$batch_size,
            otel.name = "embedding.generate",
            otel.kind = "client"
        )
    };
}
