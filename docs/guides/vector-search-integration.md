# Vector Search Integration Guide

> Guide for integrating vector search capabilities into AletheiaDB applications

## Overview

AletheiaDB provides built-in HNSW (Hierarchical Navigable Small World) indexing for fast k-nearest neighbor (k-NN) search on vector embeddings. This enables semantic similarity search, recommendation systems, and RAG (Retrieval-Augmented Generation) workflows while maintaining full bi-temporal versioning of your vector data.

**Key Features:**
- Store dense vector embeddings as first-class properties
- Automatic HNSW indexing on vector properties
- k-NN search with cosine, euclidean, or dot product metrics
- Label-based filtering for multi-tenancy or categorization
- Thread-safe concurrent operations
- Automatic index updates on node modifications

## Prerequisites

Add AletheiaDB to your `Cargo.toml`:

```toml
[dependencies]
aletheiadb = "0.1"
```

## Quick Start

### 1. Enable Vector Indexing

```rust
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use aletheiadb::index::vector::{HnswConfig, DistanceMetric};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = AletheiaDB::new();

    // Enable vector indexing on "embedding" property
    // 384 dimensions, cosine similarity, capacity for 10,000 vectors
    let config = HnswConfig::new(384, DistanceMetric::Cosine)
        .with_capacity(10000);

    db.enable_vector_index("embedding", config)?;

    Ok(())
}
```

### 2. Store Nodes with Embeddings

```rust
// Create a node with vector embedding
let embedding = vec![0.1f32, 0.2, 0.3, /* ... 381 more values */];

let doc_id = db.create_node(
    "Document",
    PropertyMapBuilder::new()
        .insert("title", "Introduction to Rust")
        .insert("content", "Rust is a systems programming language...")
        .insert_vector("embedding", &embedding)  // Vector property
        .build(),
)?;

println!("Created document: {:?}", doc_id);
```

**Auto-indexing:** The vector is automatically added to the HNSW index when the node is created. If indexing fails, the entire node creation is rolled back.

### 3. Search for Similar Nodes

#### By Node ID

```rust
// Find 10 most similar nodes to a given node
let similar = db.find_similar(doc_id, 10)?;

for (node_id, similarity) in similar {
    println!("Node {:?} has similarity {:.4}", node_id, similarity);
}
```

#### By Embedding Vector

```rust
// Search using a query embedding
let query_embedding = vec![0.15f32, 0.25, 0.35, /* ... */];
let results = db.find_similar_by_embedding(&query_embedding, 10)?;

for (node_id, similarity) in results {
    let node = db.get_node(node_id)?;
    let title = node.get_property("title")
        .and_then(|p| p.as_str())
        .unwrap_or("Untitled");

    println!("{}: {:.4}", title, similarity);
}
```

#### With Label Filter

```rust
// Find similar nodes only with "Document" label
let similar_docs = db.find_similar_with_label(doc_id, "Document", 10)?;
```

## Complete Example: Document Similarity Search

```rust
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use aletheiadb::index::vector::{HnswConfig, DistanceMetric};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Create database and enable vector index
    let db = AletheiaDB::new();

    let config = HnswConfig::new(384, DistanceMetric::Cosine)
        .with_capacity(1000);
    db.enable_vector_index("embedding", config)?;

    // 2. Insert documents with embeddings
    let documents = vec![
        ("Rust Basics", generate_embedding("rust programming basics")),
        ("Python Tutorial", generate_embedding("python programming tutorial")),
        ("Rust Advanced", generate_embedding("advanced rust patterns")),
        ("JavaScript Guide", generate_embedding("javascript guide")),
    ];

    let mut doc_ids = Vec::new();
    for (title, embedding) in documents {
        let doc_id = db.create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("title", title)
                .insert_vector("embedding", &embedding)
                .build(),
        )?;
        doc_ids.push((title, doc_id));
    }

    // 3. Find documents similar to "Rust Basics"
    let rust_basics_id = doc_ids[0].1;
    println!("\nDocuments similar to 'Rust Basics':");

    let similar = db.find_similar_with_label(rust_basics_id, "Document", 3)?;
    for (node_id, similarity) in similar {
        let node = db.get_node(node_id)?;
        let title = node.get_property("title")
            .and_then(|p| p.as_str())
            .unwrap_or("Untitled");
        println!("  {}: {:.4}", title, similarity);
    }

    Ok(())
}

// Placeholder - use your embedding model in production
fn generate_embedding(text: &str) -> Vec<f32> {
    // In production, call your embedding API (OpenAI, Cohere, etc.)
    vec![0.0; 384]
}
```

## Multi-Property Vector Indexes

AletheiaDB supports multiple vector properties per database, each with independent HNSW indexes. This enables use cases like storing different embedding types (title vs content) or multi-modal embeddings (text vs image).

### Enabling Multiple Vector Indexes

```rust
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use aletheiadb::index::vector::{HnswConfig, DistanceMetric};

let db = AletheiaDB::new();

// Enable separate indexes for different properties
db.vector_index("title_embedding")
    .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
    .enable()?;

db.vector_index("content_embedding")
    .hnsw(HnswConfig::new(768, DistanceMetric::Cosine))
    .enable()?;

db.vector_index("image_embedding")
    .hnsw(HnswConfig::new(512, DistanceMetric::Euclidean))
    .enable()?;
```

### Storing Nodes with Multiple Embeddings

```rust
let node_id = db.create_node(
    "Document",
    PropertyMapBuilder::new()
        .insert("title", "Introduction to Rust")
        .insert_vector("title_embedding", &title_emb)    // 384 dims
        .insert_vector("content_embedding", &content_emb) // 768 dims
        .insert_vector("image_embedding", &image_emb)     // 512 dims
        .build(),
)?;
```

### Querying Specific Properties

Use the `_in` suffix methods to query specific properties:

```rust
// Query by title embeddings
let by_title = db.find_similar_in("title_embedding", node_id, 10)?;

// Query by content embeddings
let by_content = db.find_similar_in("content_embedding", node_id, 10)?;

// Search by embedding vector in specific property
let results = db.find_similar_by_embedding_in(
    "content_embedding",
    &query_embedding,
    10,
)?;

// With label filter
let filtered = db.find_similar_by_embedding_in_with_label(
    "title_embedding",
    &query_embedding,
    "Document",
    10,
)?;
```

### Reranking with Different Properties

```rust
// Get initial results by title similarity
let candidates = db.find_similar_in("title_embedding", node_id, 100)?;
let candidate_ids: Vec<_> = candidates.iter().map(|(id, _)| *id).collect();

// Rerank by content similarity
let reranked = db.rank_by_similarity_in(
    "content_embedding",
    &candidate_ids,
    &query_content_embedding,
    10,
)?;
```

### Checking Property-Specific Index Status

```rust
// Check if any vector index is enabled
if db.is_vector_index_enabled() {
    println!("At least one vector index is active");
}

// Check specific property
if db.is_vector_index_enabled_for("content_embedding") {
    println!("Content embedding index is active");
}
```

## Advanced Usage

### Updating Node Embeddings

When you update a node's embedding property, the index is automatically updated:

```rust
let new_embedding = vec![0.2f32, 0.3, 0.4, /* ... */];

let mut tx = db.write_transaction()?;
tx.update_node(
    doc_id,
    PropertyMapBuilder::new()
        .insert_vector("embedding", &new_embedding)
        .build(),
)?;
tx.commit()?;

// Index automatically reflects the new embedding
```

**Rollback on failure:** If index update fails, the entire transaction is rolled back.

### Nodes Without Vectors

Nodes without the indexed vector property are silently skipped:

```rust
// This node won't be indexed (no "embedding" property)
let non_vector_id = db.create_node(
    "Metadata",
    PropertyMapBuilder::new()
        .insert("created_at", "2024-01-01")
        .build(),
)?;

// No error - node created successfully, just not indexed
```

### Checking Index Status

```rust
if db.is_vector_index_enabled() {
    println!("Vector index is active");
} else {
    println!("Vector index not enabled");
}
```

### Distance Metrics

Choose the appropriate metric for your use case:

```rust
// Cosine similarity: semantic search, text embeddings
let config = HnswConfig::new(384, DistanceMetric::Cosine);

// Euclidean distance: spatial data, image embeddings
let config = HnswConfig::new(512, DistanceMetric::Euclidean);

// Dot product: MaxSim, ColBERT-style search
let config = HnswConfig::new(128, DistanceMetric::DotProduct);
```

**Metric Characteristics:**

| Metric | Range | Normalized? | Use Case |
|--------|-------|-------------|----------|
| Cosine | [0, 1] | Yes | Text/semantic similarity |
| Euclidean | [0, ∞) | No | Spatial/geometric data |
| Dot Product | (-∞, ∞) | No | Late interaction models |

### Concurrent Operations

The vector index is thread-safe:

```rust
use std::sync::Arc;
use std::thread;

let db = Arc::new(db);

let handles: Vec<_> = (0..4).map(|i| {
    let db_clone = Arc::clone(&db);
    thread::spawn(move || {
        let embedding = generate_embedding(&format!("doc-{}", i));
        db_clone.create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("id", i)
                .insert_vector("embedding", &embedding)
                .build(),
        )
    })
}).collect();

for handle in handles {
    handle.join().unwrap()?;
}
```

## Integration with Embedding Models

### OpenAI Embeddings

```rust
use reqwest::blocking::Client;
use serde_json::json;

fn get_openai_embedding(text: &str, api_key: &str) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    let client = Client::new();

    let response = client
        .post("https://api.openai.com/v1/embeddings")
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&json!({
            "input": text,
            "model": "text-embedding-3-small"
        }))
        .send()?
        .json::<serde_json::Value>()?;

    let embedding = response["data"][0]["embedding"]
        .as_array()
        .ok_or("Invalid response")?
        .iter()
        .map(|v| v.as_f64().unwrap() as f32)
        .collect();

    Ok(embedding)
}

// Usage
let embedding = get_openai_embedding("Hello, world!", &api_key)?;
db.create_node(
    "Document",
    PropertyMapBuilder::new()
        .insert_vector("embedding", &embedding)
        .build(),
)?;
```

### Local Models (sentence-transformers)

```rust
// Use a Rust ML library like ort (ONNX Runtime)
use ort::{Session, Value};

fn get_local_embedding(text: &str, session: &Session) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    // Tokenize and run inference
    // (Implementation depends on your model)
    todo!("Implement local embedding")
}
```

## Common Embedding Dimensions

Configure your index dimensions based on your embedding model:

| Model | Provider | Dimensions | Metric |
|-------|----------|------------|--------|
| text-embedding-3-small | OpenAI | 1536 | Cosine |
| text-embedding-3-large | OpenAI | 3072 | Cosine |
| all-MiniLM-L6-v2 | Sentence Transformers | 384 | Cosine |
| all-mpnet-base-v2 | Sentence Transformers | 768 | Cosine |
| CLIP ViT-B/32 | OpenAI | 512 | Cosine |

## Error Handling

```rust
use aletheiadb::utils::VectorError;
use aletheiadb::Error;

match db.find_similar(node_id, 10) {
    Ok(results) => {
        println!("Found {} similar nodes", results.len());
    }
    Err(Error::VectorIndexNotEnabled) => {
        eprintln!("Vector index not enabled - call enable_vector_index() first");
    }
    Err(Error::PropertyNotFound { property, .. }) => {
        eprintln!("Node missing property: {}", property);
    }
    Err(Error::Vector(VectorError::DimensionMismatch { expected, actual })) => {
        eprintln!("Dimension mismatch: expected {}, got {}", expected, actual);
    }
    Err(e) => {
        eprintln!("Search failed: {}", e);
    }
}
```

## Best Practices

### 1. Choose Appropriate Dimensions

```rust
// Good: Match your embedding model
let config = HnswConfig::new(384, DistanceMetric::Cosine);

// Bad: Mismatched dimensions cause runtime errors
let config = HnswConfig::new(512, DistanceMetric::Cosine);
// Later: insert 384-dim vector -> DimensionMismatch error
```

### 2. Pre-allocate Capacity

```rust
// Good: Pre-allocate for known dataset size
let config = HnswConfig::new(384, DistanceMetric::Cosine)
    .with_capacity(100000);  // If you expect 100k documents

// OK but slower: Default capacity (requires reallocation)
let config = HnswConfig::new(384, DistanceMetric::Cosine);
```

### 3. Normalize Vectors for Cosine Similarity

```rust
use aletheiadb::core::vector::normalize;

// If using cosine metric, normalize embeddings for optimal performance
let mut embedding = get_embedding_from_model();
normalize_in_place(&mut embedding);

db.create_node(
    "Document",
    PropertyMapBuilder::new()
        .insert_vector("embedding", &embedding)
        .build(),
)?;
```

### 4. Use Label Filters

```rust
// Good: Filter by label for multi-tenant scenarios
let results = db.find_similar_with_label(query_id, "customer_123", 10)?;

// Less efficient: Filter in application code
let all_results = db.find_similar(query_id, 100)?;
let filtered: Vec<_> = all_results.into_iter()
    .filter(|(id, _)| {
        db.get_node(*id).unwrap().label() == "customer_123"
    })
    .take(10)
    .collect();
```

### 5. Batch Operations

```rust
// Good: Create nodes in batch with write transaction
let mut tx = db.write_transaction()?;

for (title, embedding) in documents {
    tx.create_node(
        "Document",
        PropertyMapBuilder::new()
            .insert("title", title)
            .insert_vector("embedding", &embedding)
            .build(),
    )?;
}

tx.commit()?;  // Single commit for all operations
```

## Next Steps

- Review [Performance Tuning Guide](vector-search-performance.md) for optimization
- See [Troubleshooting Guide](vector-search-troubleshooting.md) for common issues
- Read [VECTOR_SEARCH_DESIGN.md](../VECTOR_SEARCH_DESIGN.md) for architecture details
- Check [CLAUDE.md](../../CLAUDE.md) for development guidelines

## API Reference

### AletheiaDB Methods

```rust
impl AletheiaDB {
    /// Enable vector indexing on a specific property (builder pattern)
    pub fn vector_index(&self, property_name: &str) -> VectorIndexBuilder;

    /// Check if any vector index is enabled
    pub fn is_vector_index_enabled(&self) -> bool;

    /// Check if a specific property has a vector index
    pub fn is_vector_index_enabled_for(&self, property_name: &str) -> bool;

    // === Default property methods (uses "embedding") ===

    /// Find k most similar nodes to a given node
    pub fn find_similar(
        &self,
        query_node_id: NodeId,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>>;

    /// Find k most similar nodes with label filter
    pub fn find_similar_with_label(
        &self,
        query_node_id: NodeId,
        label: &str,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>>;

    /// Find k most similar nodes to an embedding vector
    pub fn find_similar_by_embedding(
        &self,
        query_embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>>;

    // === Property-specific methods (multi-property support) ===

    /// Find k most similar nodes in a specific property
    pub fn find_similar_in(
        &self,
        property_name: &str,
        query_node_id: NodeId,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>>;

    /// Find similar by embedding in a specific property
    pub fn find_similar_by_embedding_in(
        &self,
        property_name: &str,
        query_embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>>;

    /// Find similar by embedding with label filter in a specific property
    pub fn find_similar_by_embedding_in_with_label(
        &self,
        property_name: &str,
        query_embedding: &[f32],
        label: &str,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>>;

    /// Rerank node IDs by similarity in a specific property
    pub fn rank_by_similarity_in(
        &self,
        property_name: &str,
        node_ids: &[NodeId],
        query_embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>>;
}
```

### HnswConfig

```rust
impl HnswConfig {
    /// Create new configuration with dimensions and distance metric
    pub fn new(dimensions: usize, metric: DistanceMetric) -> Self;

    /// Set expected capacity (default: 1000)
    pub fn with_capacity(self, capacity: usize) -> Self;

    /// Set connectivity parameter M (default: 16)
    pub fn with_connectivity(self, m: usize) -> Self;

    /// Set ef_construction parameter (default: 128)
    pub fn with_expansion_add(self, ef_construction: usize) -> Self;

    /// Set ef_search parameter (default: 64)
    pub fn with_expansion_search(self, ef_search: usize) -> Self;
}
```

### Distance Metrics

```rust
pub enum DistanceMetric {
    Cosine,       // Cosine similarity: [0, 1]
    Euclidean,    // Euclidean distance: [0, ∞)
    DotProduct,   // Dot product: (-∞, ∞)
}
```
