/**
 * Dynamic SQL filter fragment builder for the SQLite backend, shared by dense,
 * sparse, and graph queries.
 */

use sqlx::QueryBuilder;
use sqlx::sqlite::Sqlite;

use crate::store::SearchFilters;

/**
 * Appends the common document filters used by dense and sparse queries.
 */
pub(crate) fn push_sqlite_document_filters<'a>(
    builder: &mut QueryBuilder<'a, Sqlite>,
    filters: &'a SearchFilters,
) {
    if let Some(project) = filters.project.as_deref() {
        builder.push(" AND d.project = ");
        builder.push_bind(project);
    }

    if let Some(file_types) = filters.file_types.as_deref()
        && !file_types.is_empty()
    {
        builder.push(" AND d.file_type IN (");
        let mut separated = builder.separated(", ");
        for file_type in file_types {
            separated.push_bind(file_type);
        }
        builder.push(")");
    }

    if let Some(tags) = filters.tags.as_deref()
        && !tags.is_empty()
    {
        builder.push(" AND EXISTS (SELECT 1 FROM json_each(d.tags) AS tag WHERE ");
        let mut separated = builder.separated(" OR ");
        for tag in tags {
            separated.push("tag.value = ");
            separated.push_bind(tag);
        }
        builder.push(")");
    }
}
