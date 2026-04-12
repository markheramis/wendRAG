pub mod ollama;
pub mod openai_compat;
pub mod provider;

pub use ollama::OllamaProvider;
pub use openai_compat::OpenAiCompatProvider;
pub use provider::EmbeddingProvider;
