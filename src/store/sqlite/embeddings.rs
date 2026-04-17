/*!
 * Embedding encode/decode, dimension validation, and cosine similarity for the
 * SQLite BLOB-based embedding storage layer.
 */

pub(crate) const EMBEDDING_DIMENSIONS: usize = 1024;

/**
 * Encodes a float embedding into a compact SQLite BLOB.
 */
pub(crate) fn embedding_to_blob(values: &[f32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

/**
 * Decodes an embedding BLOB back into a Rust float vector.
 */
pub(crate) fn decode_embedding_blob(blob: &[u8]) -> Result<Vec<f32>, sqlx::Error> {
    if !blob.len().is_multiple_of(std::mem::size_of::<f32>()) {
        return Err(sqlx::Error::Protocol(format!(
            "invalid SQLite embedding blob length: {}",
            blob.len()
        )));
    }

    Ok(blob
        .chunks_exact(std::mem::size_of::<f32>())
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

/**
 * Enforces the shipped 1024-dimension embedding contract for the SQLite
 * backend so embedding blobs remain schema-compatible.
 */
pub(crate) fn validate_embedding_dimensions(values: &[f32]) -> Result<(), sqlx::Error> {
    if values.len() == EMBEDDING_DIMENSIONS {
        Ok(())
    } else {
        Err(sqlx::Error::Protocol(format!(
            "expected {EMBEDDING_DIMENSIONS}-dimensional embeddings for SQLite, got {}",
            values.len()
        )))
    }
}

/**
 * Computes cosine similarity between two embeddings using pure Rust arithmetic.
 */
pub(crate) fn cosine_similarity(left: &[f32], right: &[f32]) -> Result<f64, sqlx::Error> {
    if left.len() != right.len() {
        return Err(sqlx::Error::Protocol(format!(
            "embedding dimension mismatch: {} != {}",
            left.len(),
            right.len()
        )));
    }

    let mut dot = 0.0f64;
    let mut left_norm = 0.0f64;
    let mut right_norm = 0.0f64;

    for (left_value, right_value) in left.iter().zip(right.iter()) {
        let left_value = *left_value as f64;
        let right_value = *right_value as f64;
        dot += left_value * right_value;
        left_norm += left_value * left_value;
        right_norm += right_value * right_value;
    }

    if left_norm == 0.0 || right_norm == 0.0 {
        return Ok(0.0);
    }

    Ok(dot / (left_norm.sqrt() * right_norm.sqrt()))
}
