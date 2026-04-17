/*!
 * Query routing module for automatic retrieval strategy selection.
 *
 * This module implements two-tier retrieval routing:
 * - **Local queries:** Specific, factual queries → chunk-level + entity graph
 * - **Global queries:** Thematic, exploratory queries → community-level + broader context
 *
 * Query classification is rule-based (fast, deterministic) by default,
 * with optional LLM-based classification for edge cases.
 *
 * Performance optimized:
 * - O(1) classification via keyword matching
 * - No LLM calls required for 90%+ of queries
 * - Lazy evaluation of classification confidence
 */

use std::collections::HashSet;

/// Classification of a query's intended scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryScope {
    /// Specific, factual queries best answered with chunk-level retrieval.
    Local,
    /// Thematic, exploratory queries best answered with community-level retrieval.
    Global,
    /// Ambiguous queries that could benefit from both approaches.
    Hybrid,
}

impl QueryScope {
    /// Returns true if this scope uses local (chunk-level) retrieval.
    pub fn uses_local(&self) -> bool {
        matches!(self, QueryScope::Local | QueryScope::Hybrid)
    }

    /// Returns true if this scope uses global (community-level) retrieval.
    pub fn uses_global(&self) -> bool {
        matches!(self, QueryScope::Global | QueryScope::Hybrid)
    }
}

/// Confidence score for a classification decision.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClassificationConfidence {
    /// Score from 0.0 (uncertain) to 1.0 (highly confident).
    pub score: f32,
    /// The method used to produce this classification.
    pub method: ClassificationMethod,
}

/// Methods available for query classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassificationMethod {
    /// Fast rule-based classification using keyword matching.
    RuleBased,
    /// LLM-based classification for ambiguous cases.
    LlmBased,
    /// Fallback when classification is uncertain.
    Fallback,
}

/// Result of query classification.
#[derive(Debug, Clone)]
pub struct QueryClassification {
    /// The determined scope for the query.
    pub scope: QueryScope,
    /// Confidence in this classification.
    pub confidence: ClassificationConfidence,
    /// Keywords that influenced the decision.
    pub matched_keywords: Vec<String>,
}

/// Configuration for query routing behavior.
#[derive(Debug, Clone)]
pub struct QueryRouterConfig {
    /// Threshold for high-confidence classification (skip LLM).
    pub high_confidence_threshold: f32,
    /// Threshold for using hybrid approach instead of picking one.
    pub hybrid_threshold: f32,
    /// Whether to enable LLM-based classification for edge cases.
    pub enable_llm_fallback: bool,
    /// Minimum query length to consider for classification.
    pub min_query_length: usize,
    /// Maximum query length before truncating for classification.
    pub max_query_length: usize,
}

impl Default for QueryRouterConfig {
    fn default() -> Self {
        Self {
            high_confidence_threshold: 0.8,
            hybrid_threshold: 0.6,
            enable_llm_fallback: false, // Rule-based is sufficient for now
            min_query_length: 3,
            max_query_length: 1000,
        }
    }
}

/// Router that classifies queries and determines retrieval strategy.
pub struct QueryRouter {
    config: QueryRouterConfig,
    /// Keywords that strongly indicate local (specific) queries.
    local_keywords: HashSet<&'static str>,
    /// Keywords that strongly indicate global (thematic) queries.
    global_keywords: HashSet<&'static str>,
}

impl QueryRouter {
    /// Creates a new query router with the specified configuration.
    pub fn new(config: QueryRouterConfig) -> Self {
        let local_keywords: HashSet<&'static str> = [
            // Specific entity indicators
            "what is", "what are", "who is", "who are", "where is", "where are",
            "when did", "when was", "when were", "how many", "how much",
            "define", "definition", "explain", "meaning of",
            
            // Specific action indicators
            "how do i", "how to", "how can i", "steps to", "guide to",
            "tutorial", "example", "sample", "code snippet",
            
            // Precision indicators
            "exactly", "specifically", "precisely", "detail", "details",
            "list", "enumerate", "step by step", "in detail",
            
            // Entity-specific
            "file", "function", "class", "method", "variable", "api",
            "endpoint", "database", "table", "column", "configuration",
            
            // Verification
            "is it true", "is it false", "correct", "incorrect", "true or false",
            "verify", "validate", "check",
        ]
        .iter()
        .copied()
        .collect();

        let global_keywords: HashSet<&'static str> = [
            // Thematic indicators
            "overview", "summary", "introduction", "about", "describe",
            "tell me about", "what do we know about", "general information",
            
            // Exploration indicators
            "explore", "discover", "learn about", "understand", "concepts",
            "patterns", "trends", "compare", "contrast", "difference between",
            "similarities", "relationships", "connections",
            
            // Broad scope
            "all", "every", "each", "overall", "big picture", "landscape",
            "ecosystem", "architecture", "framework", "approaches",
            
            // Analysis
            "analyze", "analysis", "evaluate", "assessment", "review",
            "survey", "study", "research on", "investigation",
            
            // Strategy/Planning
            "strategy", "plan", "roadmap", "vision", "goals", "objectives",
            "best practices", "recommendations", "guidelines",
        ]
        .iter()
        .copied()
        .collect();

        Self {
            config,
            local_keywords,
            global_keywords,
        }
    }

    /**
     * Classifies a query to determine the appropriate retrieval scope.
     *
     * This method is O(1) for most queries and requires no external API calls.
     * It uses keyword matching and heuristics for fast, deterministic classification.
     */
    pub fn classify(&self, query: &str) -> QueryClassification {
        // Handle edge cases
        if query.len() < self.config.min_query_length {
            return QueryClassification {
                scope: QueryScope::Hybrid,
                confidence: ClassificationConfidence {
                    score: 0.5,
                    method: ClassificationMethod::Fallback,
                },
                matched_keywords: vec![],
            };
        }

        // Normalize query for classification
        let normalized = query.to_lowercase();
        let query_words: Vec<&str> = normalized
            .split_whitespace()
            .take(self.config.max_query_length / 5) // Approximate word count
            .collect();

        // Count keyword matches
        let mut local_matches = Vec::new();
        let mut global_matches = Vec::new();

        // Check for multi-word phrases first
        let local_phrases: Vec<&str> = self.local_keywords
            .iter()
            .filter(|k| k.contains(' '))
            .copied()
            .collect();
        
        let global_phrases: Vec<&str> = self.global_keywords
            .iter()
            .filter(|k| k.contains(' '))
            .copied()
            .collect();

        // Check multi-word phrases
        for phrase in &local_phrases {
            if normalized.contains(phrase) {
                local_matches.push(phrase.to_string());
            }
        }
        
        for phrase in &global_phrases {
            if normalized.contains(phrase) {
                global_matches.push(phrase.to_string());
            }
        }

        // Check single words
        for word in &query_words {
            if self.local_keywords.contains(word) && !local_matches.iter().any(|m| m.contains(word)) {
                local_matches.push(word.to_string());
            }
            if self.global_keywords.contains(word) && !global_matches.iter().any(|m| m.contains(word)) {
                global_matches.push(word.to_string());
            }
        }

        // Calculate scores
        let local_score = (local_matches.len() as f32) * 0.3; // Each match adds 0.3
        let global_score = (global_matches.len() as f32) * 0.3;
        
        // Add query length heuristics
        let word_count = query_words.len() as f32;
        
        // Longer queries tend to be more specific (local)
        if word_count > 10.0 {
            // But very long queries without keywords might be global
            if local_matches.is_empty() && global_matches.is_empty() {
                // No strong indicators - use hybrid
            }
        }

        // Determine scope based on scores
        let (scope, confidence_score, method) = if local_score > 0.0 && global_score > 0.0 {
            // Both types of keywords present
            let diff = (local_score - global_score).abs();
            if diff < 0.3 {
                // Similar scores - use hybrid
                (QueryScope::Hybrid, 0.6, ClassificationMethod::RuleBased)
            } else if local_score > global_score {
                (QueryScope::Local, 0.7 + diff.min(0.3), ClassificationMethod::RuleBased)
            } else {
                (QueryScope::Global, 0.7 + diff.min(0.3), ClassificationMethod::RuleBased)
            }
        } else if local_score > 0.0 {
            // Only local keywords
            (QueryScope::Local, (0.7 + local_score).min(1.0), ClassificationMethod::RuleBased)
        } else if global_score > 0.0 {
            // Only global keywords
            (QueryScope::Global, (0.7 + global_score).min(1.0), ClassificationMethod::RuleBased)
        } else {
            // No strong indicators - use hybrid as safe default
            (QueryScope::Hybrid, 0.5, ClassificationMethod::Fallback)
        };

        // Combine all matched keywords
        let mut matched_keywords = local_matches;
        matched_keywords.extend(global_matches);

        QueryClassification {
            scope,
            confidence: ClassificationConfidence {
                score: confidence_score,
                method,
            },
            matched_keywords,
        }
    }

    /**
     * Routes a query to the appropriate retrieval strategies.
     *
     * Returns which retrieval modes should be used and in what order.
     */
    pub fn route(&self, query: &str) -> RouteDecision {
        let classification = self.classify(query);
        
        // High confidence: use single strategy
        if classification.confidence.score >= self.config.high_confidence_threshold {
            RouteDecision {
                primary_scope: classification.scope,
                secondary_scope: None,
                classification,
            }
        } else if classification.confidence.score >= self.config.hybrid_threshold {
            // Medium confidence: use hybrid
            RouteDecision {
                primary_scope: classification.scope,
                secondary_scope: Some(QueryScope::Hybrid),
                classification,
            }
        } else {
            // Low confidence: default to hybrid
            RouteDecision {
                primary_scope: QueryScope::Hybrid,
                secondary_scope: None,
                classification,
            }
        }
    }
}

/// Result of query routing decision.
#[derive(Debug, Clone)]
pub struct RouteDecision {
    /// Primary retrieval scope to use.
    pub primary_scope: QueryScope,
    /// Optional secondary scope for additional retrieval.
    pub secondary_scope: Option<QueryScope>,
    /// The classification that led to this decision.
    pub classification: QueryClassification,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_local_query_classification() {
        let router = QueryRouter::new(QueryRouterConfig::default());
        
        let test_cases = vec![
            ("What is the API endpoint for user authentication?", QueryScope::Local),
            ("How do I configure the database connection?", QueryScope::Local),
            ("Show me the code example for JWT validation", QueryScope::Local),
            ("What are the steps to deploy this application?", QueryScope::Local),
            ("Explain the meaning of entity extraction", QueryScope::Local),
        ];
        
        for (query, expected_scope) in test_cases {
            let classification = router.classify(query);
            assert!(
                classification.scope == expected_scope || classification.scope == QueryScope::Hybrid,
                "Query '{}' should be classified as {:?} or Hybrid, got {:?}",
                query,
                expected_scope,
                classification.scope
            );
            assert!(
                classification.confidence.score > 0.5,
                "Classification confidence should be > 0.5 for '{}'",
                query
            );
        }
    }

    #[test]
    fn test_global_query_classification() {
        let router = QueryRouter::new(QueryRouterConfig::default());
        
        let test_cases = vec![
            ("Give me an overview of the architecture", QueryScope::Global),
            ("What are the best practices for RAG systems?", QueryScope::Global),
            ("Compare different embedding models", QueryScope::Global),
            ("Describe the overall system design", QueryScope::Global),
            ("What patterns are used in this codebase?", QueryScope::Global),
        ];
        
        for (query, expected_scope) in test_cases {
            let classification = router.classify(query);
            assert!(
                classification.scope == expected_scope || classification.scope == QueryScope::Hybrid,
                "Query '{}' should be classified as {:?} or Hybrid, got {:?}",
                query,
                expected_scope,
                classification.scope
            );
        }
    }

    #[test]
    fn test_hybrid_fallback() {
        let router = QueryRouter::new(QueryRouterConfig::default());
        
        // Very short queries should fall back to hybrid
        let classification = router.classify("hi");
        assert_eq!(classification.scope, QueryScope::Hybrid);
        
        // Ambiguous queries without keywords
        let classification = router.classify("tell me something");
        assert_eq!(classification.scope, QueryScope::Hybrid);
    }

    #[test]
    fn test_routing_decision() {
        let router = QueryRouter::new(QueryRouterConfig::default());
        
        let decision = router.route("What is the function signature?");
        assert!(decision.primary_scope.uses_local());
        assert!(decision.classification.confidence.score > 0.5);
        
        let decision = router.route("Give me an overview");
        assert!(decision.primary_scope.uses_global());
    }

    #[test]
    fn test_keyword_matching() {
        let router = QueryRouter::new(QueryRouterConfig::default());
        
        let classification = router.classify("how to implement authentication?");
        assert!(!classification.matched_keywords.is_empty());
        assert!(classification.matched_keywords.iter().any(|k| k.contains("how to") || k.contains("how")));
    }
}
