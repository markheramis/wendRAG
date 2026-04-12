/**
 * Example program to inspect how semantic chunking processes a document.
 * Run with: cargo run --example inspect_chunks -- <path_to_file>
 */

use std::env;
use std::fs;
use std::sync::Arc;

use wend_rag::config::ChunkingStrategy;
use wend_rag::embed::{EmbeddingProvider, provider::EmbeddingError};
use wend_rag::ingest::chunker::chunk_document;

/// Mock embedder for testing that creates embeddings based on content
struct DebugEmbedder;

#[async_trait::async_trait]
impl EmbeddingProvider for DebugEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        let mut embeddings = Vec::new();
        
        for text in texts {
            let mut embedding = vec![0.0f32; 384];
            let words: Vec<&str> = text.split_whitespace().collect();
            
            // Topic-based embedding generation
            let topics = [
                ("prompt", 0), ("agent", 0), ("clarify", 0),
                ("plan", 50), ("approval", 50), ("gate", 50),
                ("tool", 100), ("router", 100), ("route", 100),
                ("department", 150), ("delegate", 150), ("subagent", 150),
                ("reasoning", 200), ("thinking", 200), ("debug", 200),
                ("safety", 250), ("terminal", 250), ("file", 250),
                ("constitution", 300), ("principle", 300), ("rule", 300),
            ];
            
            for (word, pos) in &topics {
                if words.iter().any(|w| w.to_lowercase().contains(word)) {
                    embedding[*pos] = 1.0;
                    if *pos > 0 { embedding[pos - 1] = 0.3; }
                    if *pos < 383 { embedding[pos + 1] = 0.3; }
                }
            }
            
            // Normalize
            let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for val in &mut embedding {
                    *val /= norm;
                }
            }
            
            embeddings.push(embedding);
        }
        
        Ok(embeddings)
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    let file_path = args.get(1).expect("Usage: inspect_chunks <path_to_file>");
    
    let content = fs::read_to_string(file_path)
        .expect(&format!("Failed to read file: {}", file_path));
    
    let embedder: Arc<dyn EmbeddingProvider> = Arc::new(DebugEmbedder);
    
    println!("========================================");
    println!("Processing: {}", file_path);
    println!("Total characters: {}", content.len());
    println!("========================================\n");
    
    // Test FIXED chunking
    println!("--- FIXED CHUNKING ---");
    let fixed_chunks = chunk_document(
        &content,
        "markdown",
        ChunkingStrategy::Fixed,
        Some(&embedder),
        0.25,
        20,
        false,
    )
    .await
    .expect("Fixed chunking failed");
    
    println!("Fixed chunks produced: {}\n", fixed_chunks.len());
    for (i, chunk) in fixed_chunks.iter().enumerate().take(5) {
        let preview = &chunk.content[..chunk.content.len().min(120)];
        println!("Fixed Chunk {} ({} chars): {}", i, chunk.content.len(), preview);
    }
    if fixed_chunks.len() > 5 {
        println!("... and {} more chunks", fixed_chunks.len() - 5);
    }
    
    // Test SEMANTIC chunking with different thresholds
    for threshold in [0.25, 0.50] {
        println!("\n--- SEMANTIC CHUNKING (threshold={}) ---", threshold);
        let semantic_chunks = chunk_document(
            &content,
            "markdown",
            ChunkingStrategy::Semantic,
            Some(&embedder),
            threshold,
            20,
            false,
        )
        .await
        .expect("Semantic chunking failed");
        
        println!("Semantic chunks produced: {}", semantic_chunks.len());
        for (i, chunk) in semantic_chunks.iter().enumerate() {
            let preview = &chunk.content[..chunk.content.len().min(120)];
            println!("\nSemantic Chunk {} ({} chars):", i, chunk.content.len());
            println!("  Preview: {}", preview);
            if let Some(title) = &chunk.section_title {
                println!("  Section: {}", title);
            }
        }
    }
    
    // Test with garbage filtering
    println!("\n--- SEMANTIC CHUNKING WITH GARBAGE FILTER ---");
    let filtered_chunks = chunk_document(
        &content,
        "markdown",
        ChunkingStrategy::Semantic,
        Some(&embedder),
        0.25,
        20,
        true, // filter garbage
    )
    .await
    .expect("Filtered chunking failed");
    
    println!("Filtered chunks produced: {}", filtered_chunks.len());
    
    println!("\n========================================");
    println!("Chunking comparison:");
    println!("  Fixed: {} chunks", fixed_chunks.len());
    println!("  Semantic (0.25): produced chunks", );
    println!("  Semantic + filter: {} chunks", filtered_chunks.len());
    println!("========================================");
}
