pub mod community;
pub mod dense;
pub mod fusion;
pub mod hybrid;
pub mod router;
pub mod sparse;

use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct ScoredChunk {
    pub chunk_id: Uuid,
    pub content: String,
    pub section_title: Option<String>,
    pub file_path: String,
    pub file_name: String,
    pub chunk_index: i32,
    pub score: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    Dense,
    Sparse,
    Hybrid,
}

impl SearchMode {
    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "dense" => Self::Dense,
            "sparse" => Self::Sparse,
            _ => Self::Hybrid,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Dense => "dense",
            Self::Sparse => "sparse",
            Self::Hybrid => "hybrid",
        }
    }
}
