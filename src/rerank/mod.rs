/**
 * Reranking module.
 *
 * Provides an optional post-retrieval reranking stage that improves precision
 * by scoring candidates with a cross-encoder or dedicated rerank API. Supports
 * Cohere Rerank, Jina Reranker, and any OpenAI-compatible `/v1/rerank`
 * endpoint (Infinity, TEI, vLLM, etc.).
 *
 * The module is structured as a provider trait with pluggable implementations,
 * mirroring the `embed` module pattern.
 */

pub mod cohere;
pub mod jina;
pub mod openai_compat;
pub mod provider;

pub use cohere::CohereReranker;
pub use jina::JinaReranker;
pub use openai_compat::OpenAiCompatReranker;
pub use provider::{RerankResult, RerankerError, RerankerProvider};

/// Supported reranker provider backends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RerankerProviderKind {
    Cohere,
    Jina,
    OpenAiCompatible,
}

impl RerankerProviderKind {
    /**
     * Parses a provider name string into the corresponding enum variant.
     * Accepts common aliases (`cohere`, `jina`, `openai-compatible`).
     * Returns `None` for unrecognised values.
     */
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "cohere" => Some(Self::Cohere),
            "jina" => Some(Self::Jina),
            "openai-compatible" | "openai_compatible" | "openai-compat" => {
                Some(Self::OpenAiCompatible)
            }
            _ => None,
        }
    }
}

/// Runtime configuration for the reranking stage.
#[derive(Debug, Clone)]
pub struct RerankerConfig {
    pub enabled: bool,
    pub provider: RerankerProviderKind,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// How many candidates to pass to the reranker. The retrieval stage
    /// over-fetches this many results, then the reranker trims to the
    /// caller's requested `top_k`.
    pub top_n: usize,
}

/// Default over-fetch multiplier: retrieve 3× the requested top_k, then
/// rerank down to the final count.
pub const DEFAULT_RERANKER_TOP_N: usize = 30;

/**
 * Factory: builds a boxed `RerankerProvider` from the runtime configuration.
 *
 * # Parameters
 * - `config`: The reranker configuration parsed from env/YAML.
 *
 * # Returns
 * A trait-object reranker ready for use in the search pipeline.
 */
pub fn build_reranker(config: &RerankerConfig) -> Box<dyn RerankerProvider> {
    match config.provider {
        RerankerProviderKind::Cohere => Box::new(CohereReranker::new(
            config.base_url.clone(),
            config.api_key.clone(),
            config.model.clone(),
        )),
        RerankerProviderKind::Jina => Box::new(JinaReranker::new(
            config.base_url.clone(),
            config.api_key.clone(),
            config.model.clone(),
        )),
        RerankerProviderKind::OpenAiCompatible => Box::new(OpenAiCompatReranker::new(
            config.base_url.clone(),
            config.api_key.clone(),
            config.model.clone(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /**
     * Verifies that all expected provider name variants are parsed correctly.
     */
    #[test]
    fn provider_kind_parsing() {
        assert_eq!(
            RerankerProviderKind::from_str("cohere"),
            Some(RerankerProviderKind::Cohere)
        );
        assert_eq!(
            RerankerProviderKind::from_str("jina"),
            Some(RerankerProviderKind::Jina)
        );
        assert_eq!(
            RerankerProviderKind::from_str("openai-compatible"),
            Some(RerankerProviderKind::OpenAiCompatible)
        );
        assert_eq!(
            RerankerProviderKind::from_str("openai_compatible"),
            Some(RerankerProviderKind::OpenAiCompatible)
        );
        assert_eq!(
            RerankerProviderKind::from_str("openai-compat"),
            Some(RerankerProviderKind::OpenAiCompatible)
        );
        assert_eq!(RerankerProviderKind::from_str("unknown"), None);
    }

    /**
     * Verifies that the factory function produces a provider for each kind
     * without panicking.
     */
    #[test]
    fn build_reranker_produces_all_variants() {
        for kind in [
            RerankerProviderKind::Cohere,
            RerankerProviderKind::Jina,
            RerankerProviderKind::OpenAiCompatible,
        ] {
            let config = RerankerConfig {
                enabled: true,
                provider: kind,
                base_url: String::new(),
                api_key: "test-key".into(),
                model: "test-model".into(),
                top_n: 30,
            };
            let _provider = build_reranker(&config);
        }
    }
}
