/**
 * Text normalization and cleaning utilities shared by entity extraction,
 * graph aggregation, and storage backends.
 */

const DEFAULT_ENTITY_TYPE: &str = "CONCEPT";

/**
 * Produces the normalized entity-name key used for backend deduplication.
 */
pub(crate) fn normalize_entity_name(value: &str) -> String {
    value
        .trim()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/**
 * Produces the canonical uppercase entity type used in storage and retrieval.
 */
pub(crate) fn normalize_entity_type(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        DEFAULT_ENTITY_TYPE.to_string()
    } else {
        trimmed
            .chars()
            .map(|character| match character {
                'a'..='z' => character.to_ascii_uppercase(),
                'A'..='Z' | '0'..='9' => character,
                _ => '_',
            })
            .collect()
    }
}

/**
 * Normalizes relationship labels into a stable uppercase form suitable for
 * persistence and recursive graph traversal.
 */
pub(crate) fn normalize_relationship_type(value: &str) -> String {
    normalize_entity_type(value)
}

/**
 * Cleans a required free-text field and drops it when nothing meaningful
 * remains after trimming.
 */
pub(crate) fn clean_required_text(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/**
 * Cleans an optional free-text field while preserving the absent/empty
 * distinction used by the extractor models.
 */
pub(crate) fn clean_optional_text(value: Option<&str>) -> Option<String> {
    value.and_then(|text| {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

/**
 * Returns the default entity type sentinel used when the extractor omits a
 * type for an entity or relationship endpoint.
 */
pub(crate) fn default_entity_type() -> &'static str {
    DEFAULT_ENTITY_TYPE
}
