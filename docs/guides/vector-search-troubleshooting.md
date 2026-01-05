# Vector Search Troubleshooting Guide

> Common issues and solutions for vector search in GallifreyDB

## Overview

This guide covers common problems, error messages, and debugging techniques for GallifreyDB's vector search functionality. If you don't find your issue here, check the [Integration Guide](vector-search-integration.md) or [Performance Guide](vector-search-performance.md).

## Quick Diagnosis

| Symptom | Likely Cause | Section |
|---------|--------------|---------|
| "VectorIndexNotEnabled" error | Index not configured | [Index Not Enabled](#error-vectorindexnotenabled) |
| "DimensionMismatch" error | Wrong embedding size | [Dimension Mismatch](#error-dimensionmismatch) |
| "PropertyNotFound" error | Missing vector property | [Missing Property](#error-propertynotfound) |
| Empty search results | No vectors indexed | [No Results](#issue-no-search-results) |
| Slow queries (>10ms) | Poor configuration | [Slow Queries](#issue-slow-query-performance) |
| High memory usage | M parameter too high | [Memory Issues](#issue-high-memory-usage) |
| Index build failures | Invalid vectors (NaN/Inf) | [Invalid Vectors](#error-invalidvector) |
| Thread panics | Concurrent access bug | [Concurrency Issues](#issue-concurrent-operations-failing) |

## Common Errors

### Error: VectorIndexNotEnabled

**Error Message:**
```
Error: VectorIndexNotEnabled
```

**Cause:** Attempting to perform vector search without enabling the index.

**Solution:**

```rust
use gallifreydb::index::vector::{HnswConfig, DistanceMetric};

// Enable index BEFORE performing searches
let config = HnswConfig::new(384, DistanceMetric::Cosine);
db.enable_vector_index("embedding", config)?;

// Now searches will work
let results = db.find_similar(node_id, 10)?;
```

**Check if index is enabled:**
```rust
if !db.is_vector_index_enabled() {
    eprintln!("Vector index is not enabled!");
    // Enable it or return error
}
```

---

### Error: DimensionMismatch

**Error Message:**
```
Error: Vector(DimensionMismatch { expected: 384, actual: 768 })
```

**Cause:** Vector dimensions don't match the index configuration.

**Common scenarios:**
1. Index configured for one model, using embeddings from another
2. Embedding model changed without updating index
3. Test vectors generated with wrong dimensions

**Solution:**

```rust
// ❌ WRONG: Mismatched dimensions
let config = HnswConfig::new(384, DistanceMetric::Cosine);
db.enable_vector_index("embedding", config)?;

let wrong_embedding = vec![0.0f32; 768];  // 768 dims, but index expects 384!
db.create_node(
    "Document",
    PropertyMapBuilder::new()
        .insert_vector("embedding", &wrong_embedding)
        .build(),
)?;  // Error: DimensionMismatch

// ✅ CORRECT: Matching dimensions
let config = HnswConfig::new(384, DistanceMetric::Cosine);
db.enable_vector_index("embedding", config)?;

let correct_embedding = vec![0.0f32; 384];  // 384 dims matches config
db.create_node(
    "Document",
    PropertyMapBuilder::new()
        .insert_vector("embedding", &correct_embedding)
        .build(),
)?;  // Success
```

**Validation before indexing:**
```rust
use gallifreydb::core::vector::validate_vector_with_bounds;

let embedding = get_embedding_from_model();

// Validate dimensions before creating node
if embedding.len() != expected_dimensions {
    return Err(format!(
        "Expected {} dimensions, got {}",
        expected_dimensions,
        embedding.len()
    ).into());
}
```

---

### Error: PropertyNotFound

**Error Message:**
```
Error: PropertyNotFound { node_id: NodeId(42), property: "embedding" }
```

**Cause:** Node doesn't have the indexed vector property.

**Solution:**

```rust
// ❌ WRONG: Query node without vector property
let metadata_node = db.create_node(
    "Metadata",
    PropertyMapBuilder::new()
        .insert("created_at", "2024-01-01")
        // No "embedding" property!
        .build(),
)?;

db.find_similar(metadata_node, 10)?;  // Error: PropertyNotFound

// ✅ CORRECT: Only query nodes with vector property
let vector_node = db.create_node(
    "Document",
    PropertyMapBuilder::new()
        .insert("title", "My Doc")
        .insert_vector("embedding", &embedding)
        .build(),
)?;

db.find_similar(vector_node, 10)?;  // Success
```

**Check if property exists before querying:**
```rust
let node = db.get_node(node_id)?;

if node.get_property("embedding").is_some() {
    let results = db.find_similar(node_id, 10)?;
} else {
    eprintln!("Node {:?} has no embedding", node_id);
}
```

---

### Error: InvalidVector

**Error Message:**
```
Error: Vector(InvalidVector("Vector contains NaN values"))
Error: Vector(InvalidVector("Vector contains infinite values"))
```

**Cause:** Vector contains NaN (Not a Number) or Infinity values.

**Common scenarios:**
1. Division by zero during normalization
2. Corrupted embedding from model
3. Arithmetic overflow/underflow

**Solution:**

```rust
use gallifreydb::core::vector::{validate_vector, normalize_in_place};

let mut embedding = get_embedding_from_model();

// Validate before using
if let Err(e) = validate_vector(&embedding) {
    eprintln!("Invalid embedding: {}", e);
    // Handle error: skip, use default, or return error
    return Err(e);
}

// Safe to use
db.create_node(
    "Document",
    PropertyMapBuilder::new()
        .insert_vector("embedding", &embedding)
        .build(),
)?;
```

**Handling zero-magnitude vectors:**
```rust
use gallifreydb::core::vector::{magnitude, normalize_in_place};

let mut embedding = get_embedding_from_model();
let mag = magnitude(&embedding);

if mag < 1e-10 {
    // Zero or near-zero vector - can't normalize
    eprintln!("Warning: Zero-magnitude vector, using default");
    embedding = vec![1.0 / (embedding.len() as f32).sqrt(); embedding.len()];
} else {
    normalize_in_place(&mut embedding);
}
```

---

### Error: VectorIndexAlreadyEnabled

**Error Message:**
```
Error: VectorIndexAlreadyEnabled
```

**Cause:** Attempting to enable vector index when it's already enabled.

**Solution:**

```rust
// ❌ WRONG: Enable twice
let config = HnswConfig::new(384, DistanceMetric::Cosine);
db.enable_vector_index("embedding", config.clone())?;
db.enable_vector_index("embedding", config)?;  // Error!

// ✅ CORRECT: Check before enabling
if !db.is_vector_index_enabled() {
    let config = HnswConfig::new(384, DistanceMetric::Cosine);
    db.enable_vector_index("embedding", config)?;
}
```

**Note:** Currently GallifreyDB supports one vector index per database. Multi-index support planned for future releases.

---

### Error: NodeNotFound

**Error Message:**
```
Error: NodeNotFound(NodeId(99))
```

**Cause:** Attempting to query with a node ID that doesn't exist.

**Solution:**

```rust
// Verify node exists before querying
match db.get_node(node_id) {
    Ok(_) => {
        let results = db.find_similar(node_id, 10)?;
    }
    Err(_) => {
        eprintln!("Node {:?} not found", node_id);
    }
}
```

## Common Issues

### Issue: No Search Results

**Symptoms:** `find_similar()` returns empty vector or only self.

**Diagnosis:**

```rust
let results = db.find_similar(node_id, 10)?;
println!("Found {} results", results.len());  // Prints 0 or 1

// Check if any nodes are indexed
let test_embedding = vec![0.5f32; 384];
let all_results = db.find_similar_by_embedding(&test_embedding, 100)?;
println!("Total indexed nodes: {}", all_results.len());
```

**Common Causes:**

1. **No vectors indexed**
   ```rust
   // Problem: Index enabled but no nodes created with vectors
   let config = HnswConfig::new(384, DistanceMetric::Cosine);
   db.enable_vector_index("embedding", config)?;

   // Created nodes without vector property
   db.create_node("Doc", PropertyMapBuilder::new()
       .insert("title", "Test")
       .build())?;  // Not indexed!

   // Solution: Add vector property
   db.create_node("Doc", PropertyMapBuilder::new()
       .insert("title", "Test")
       .insert_vector("embedding", &embedding)
       .build())?;  // Now indexed
   ```

2. **Label filter too restrictive**
   ```rust
   // Problem: Label doesn't match any nodes
   let results = db.find_similar_with_label(node_id, "NonExistentLabel", 10)?;

   // Solution: Check label spelling and existence
   let results = db.find_similar_with_label(node_id, "Document", 10)?;
   ```

3. **k larger than dataset**
   ```rust
   // Problem: Only 5 nodes indexed, requesting 100
   let results = db.find_similar(node_id, 100)?;
   println!("{} results", results.len());  // Prints 5 (or 4 excluding self)

   // Solution: Request realistic k value
   let results = db.find_similar(node_id, 5)?;
   ```

---

### Issue: Slow Query Performance

**Symptoms:** Queries taking >10ms for <100k vectors.

**Diagnosis:**

```rust
use std::time::Instant;

let start = Instant::now();
let results = db.find_similar(node_id, 10)?;
let latency = start.elapsed();
println!("Query took: {:?}", latency);
```

**Common Causes:**

1. **ef_search too high**
   ```rust
   // Problem: Excessive candidate exploration
   let config = HnswConfig::new(384, DistanceMetric::Cosine)
       .with_expansion_search(500);  // Way too high!

   // Solution: Use reasonable ef_search
   let config = HnswConfig::new(384, DistanceMetric::Cosine)
       .with_expansion_search(64);  // Much better
   ```

2. **Large k value**
   ```rust
   // Problem: Requesting too many results
   let results = db.find_similar(node_id, 1000)?;

   // Solution: Request only what you need
   let results = db.find_similar(node_id, 10)?;
   ```

3. **Non-normalized vectors with Cosine metric**
   ```rust
   use gallifreydb::core::vector::normalize_in_place;

   // Problem: Normalization on every distance computation
   let embedding = get_embedding_from_model();
   db.create_node(/* ... */, &embedding)?;

   // Solution: Pre-normalize
   let mut embedding = get_embedding_from_model();
   normalize_in_place(&mut embedding);
   db.create_node(/* ... */, &embedding)?;
   ```

See [Performance Guide](vector-search-performance.md) for detailed optimization.

---

### Issue: High Memory Usage

**Symptoms:** RSS growing beyond expected dataset size.

**Diagnosis:**

```rust
// Calculate expected memory usage
let num_vectors = 100_000;
let dimensions = 384;
let m = 16;

let vector_data_mb = num_vectors * dimensions * 4 / 1_048_576;
let hnsw_graph_mb = num_vectors * m * 8 / 1_048_576;
let total_mb = vector_data_mb + hnsw_graph_mb;

println!("Expected memory: ~{} MB", total_mb);
// Expected: ~146 MB for 100k vectors (384 dims, M=16)
```

**Common Causes:**

1. **M parameter too high**
   ```rust
   // Problem: Excessive connections
   let config = HnswConfig::new(384, DistanceMetric::Cosine)
       .with_connectivity(128);  // 128 connections per node!

   // Solution: Use reasonable M
   let config = HnswConfig::new(384, DistanceMetric::Cosine)
       .with_connectivity(16);  // Much more reasonable
   ```

2. **High dimensions**
   ```rust
   // Problem: Very high dimensional embeddings
   let config = HnswConfig::new(3072, DistanceMetric::Cosine);  // Large!

   // Solution: Consider dimension reduction
   let config = HnswConfig::new(384, DistanceMetric::Cosine);
   ```

3. **Capacity over-allocation**
   ```rust
   // Problem: Pre-allocated for 10M but only using 10k
   let config = HnswConfig::new(384, DistanceMetric::Cosine)
       .with_capacity(10_000_000);

   // Solution: Set realistic capacity
   let config = HnswConfig::new(384, DistanceMetric::Cosine)
       .with_capacity(10_000);
   ```

---

### Issue: Inaccurate Search Results

**Symptoms:** Results not semantically similar to query.

**Diagnosis:**

1. **Check embedding quality:**
   ```rust
   // Verify embeddings are from correct model
   let embedding = get_embedding_from_model();
   println!("Embedding dimensions: {}", embedding.len());
   println!("Magnitude: {}", magnitude(&embedding));
   ```

2. **Check distance metric:**
   ```rust
   use gallifreydb::core::vector::{cosine_similarity, euclidean_distance};

   let emb1 = get_embedding("cat");
   let emb2 = get_embedding("dog");
   let emb3 = get_embedding("car");

   let sim_cat_dog = cosine_similarity(&emb1, &emb2)?;
   let sim_cat_car = cosine_similarity(&emb1, &emb3)?;

   // cat-dog should be more similar than cat-car
   assert!(sim_cat_dog > sim_cat_car);
   ```

**Common Causes:**

1. **Wrong distance metric**
   ```rust
   // Problem: Using Euclidean for normalized embeddings
   let config = HnswConfig::new(384, DistanceMetric::Euclidean);

   // Solution: Use Cosine for semantic similarity
   let config = HnswConfig::new(384, DistanceMetric::Cosine);
   ```

2. **Low ef_search**
   ```rust
   // Problem: Not exploring enough candidates
   let config = HnswConfig::new(384, DistanceMetric::Cosine)
       .with_expansion_search(10);  // Too low!

   // Solution: Increase ef_search
   let config = HnswConfig::new(384, DistanceMetric::Cosine)
       .with_expansion_search(64);
   ```

3. **Embedding model mismatch**
   ```rust
   // Problem: Mixing embeddings from different models
   let emb1 = get_openai_embedding("text1");
   let emb2 = get_bert_embedding("text2");  // Different model!

   // Solution: Use consistent embedding model
   let emb1 = get_openai_embedding("text1");
   let emb2 = get_openai_embedding("text2");
   ```

---

### Issue: Concurrent Operations Failing

**Symptoms:** Thread panics, deadlocks, or inconsistent results.

**Diagnosis:**

```rust
use std::sync::Arc;
use std::thread;

let db = Arc::new(db);

// Test concurrent access
let handles: Vec<_> = (0..4).map(|i| {
    let db = Arc::clone(&db);
    thread::spawn(move || {
        db.find_similar_by_embedding(&vec![0.0; 384], 10)
    })
}).collect();

for handle in handles {
    match handle.join() {
        Ok(Ok(_)) => println!("Thread succeeded"),
        Ok(Err(e)) => eprintln!("Thread error: {}", e),
        Err(_) => eprintln!("Thread panicked!"),
    }
}
```

**Common Causes:**

1. **Index disabled during query**
   ```rust
   // Problem: Thread safety issue (should not happen in current impl)
   // This is a bug if it occurs - please report!
   ```

2. **Sharing mutable references**
   ```rust
   // ❌ WRONG: Sharing &mut across threads
   let mut db = GallifreyDB::new();
   // Can't share mutable reference across threads

   // ✅ CORRECT: Use Arc for shared ownership
   let db = Arc::new(GallifreyDB::new());
   // Can clone Arc and share across threads
   ```

**Current Implementation:** Vector index uses `parking_lot::RwLock` for thread-safe access. If you encounter concurrency issues, please report them as bugs.

---

### Issue: Index Build Failures

**Symptoms:** Errors during batch node creation.

**Diagnosis:**

```rust
let embeddings = load_embeddings_from_file();

for (i, embedding) in embeddings.iter().enumerate() {
    match db.create_node(
        "Document",
        PropertyMapBuilder::new()
            .insert_vector("embedding", embedding)
            .build(),
    ) {
        Ok(node_id) => println!("Created node {}: {:?}", i, node_id),
        Err(e) => eprintln!("Failed to create node {}: {}", i, e),
    }
}
```

**Common Causes:**

1. **Invalid vectors in batch**
   ```rust
   use gallifreydb::core::vector::validate_vector;

   // Pre-validate embeddings
   for (i, embedding) in embeddings.iter().enumerate() {
       if let Err(e) = validate_vector(embedding) {
           eprintln!("Invalid embedding at index {}: {}", i, e);
           // Skip or handle invalid embedding
       }
   }
   ```

2. **Transaction rollback**
   ```rust
   // Problem: One failure rolls back entire batch
   let mut tx = db.write_transaction()?;
   for embedding in embeddings {
       tx.create_node(/* ... */)?;  // Any failure rolls back all
   }
   tx.commit()?;

   // Solution: Handle errors individually
   for embedding in embeddings {
       match db.create_node(/* ... */) {
           Ok(_) => continue,
           Err(e) => eprintln!("Skipping invalid embedding: {}", e),
       }
   }
   ```

## Debugging Techniques

### Enable Debug Logging

```rust
env_logger::init();

// Set RUST_LOG environment variable:
// RUST_LOG=gallifreydb=debug cargo run
```

### Inspect Index State

```rust
// Check if index is enabled
assert!(db.is_vector_index_enabled());

// Count indexed nodes (indirect)
let probe = vec![0.0f32; 384];
let all_results = db.find_similar_by_embedding(&probe, usize::MAX)?;
println!("Indexed nodes: {}", all_results.len());
```

### Validate Embedding Quality

```rust
use gallifreydb::core::vector::{
    magnitude,
    is_normalized,
    cosine_similarity,
};

let embedding = get_embedding_from_model();

// Check magnitude
let mag = magnitude(&embedding);
println!("Magnitude: {}", mag);

// Check if normalized
if is_normalized(&embedding, 1e-6) {
    println!("Embedding is normalized");
} else {
    println!("Embedding is NOT normalized (mag: {})", mag);
}

// Check for degenerate embeddings
if mag < 1e-10 {
    eprintln!("WARNING: Near-zero embedding!");
}

// Check similarity to itself (should be 1.0)
let self_sim = cosine_similarity(&embedding, &embedding)?;
println!("Self-similarity: {}", self_sim);
assert!((self_sim - 1.0).abs() < 1e-6);
```

### Benchmark Your Workload

```rust
use std::time::Instant;

// Benchmark search
let start = Instant::now();
for _ in 0..100 {
    db.find_similar(node_id, 10)?;
}
let avg_latency = start.elapsed() / 100;
println!("Average query latency: {:?}", avg_latency);

// Benchmark insert
let start = Instant::now();
for i in 0..100 {
    db.create_node(
        "Document",
        PropertyMapBuilder::new()
            .insert_vector("embedding", &embeddings[i])
            .build(),
    )?;
}
let avg_insert = start.elapsed() / 100;
println!("Average insert latency: {:?}", avg_insert);
```

## Getting Help

### Before Reporting Issues

1. **Check error message** against this guide
2. **Verify configuration** matches your use case
3. **Test with minimal example** to isolate issue
4. **Check GallifreyDB version** - ensure you're up-to-date

### Reporting Bugs

Include in your bug report:

```rust
// 1. GallifreyDB version
println!("gallifreydb version: 0.1.0");

// 2. Configuration
println!("Dimensions: 384");
println!("Metric: Cosine");
println!("M: 16");
println!("ef_construction: 128");
println!("ef_search: 64");

// 3. Dataset size
println!("Indexed nodes: ~10,000");

// 4. Error message (copy exact text)
println!("Error: ...");

// 5. Minimal reproducible example
```

### Community Resources

- **GitHub Issues**: https://github.com/yourusername/gallifreydb/issues
- **Documentation**: [Integration Guide](vector-search-integration.md), [Performance Guide](vector-search-performance.md)
- **Design Document**: [VECTOR_SEARCH_DESIGN.md](../VECTOR_SEARCH_DESIGN.md)

## Quick Reference

### Error Checklist

When encountering an error:

- [ ] Is vector index enabled? (`db.is_vector_index_enabled()`)
- [ ] Do dimensions match config? (validate embedding length)
- [ ] Does query node have vector property? (check with `get_property()`)
- [ ] Are vectors valid? (no NaN/Inf, use `validate_vector()`)
- [ ] Is k value reasonable? (not larger than dataset)
- [ ] Are vectors normalized (if using Cosine)? (use `normalize_in_place()`)

### Performance Checklist

When queries are slow:

- [ ] Is `ef_search` reasonable? (64 is default, 32 for speed, 128 for accuracy)
- [ ] Is k value minimal? (request only what you need)
- [ ] Are vectors pre-normalized? (for Cosine metric)
- [ ] Is M parameter reasonable? (16 is default, 8 for memory, 32 for accuracy)
- [ ] Is dataset size expected? (check indexed node count)

### Memory Checklist

When memory usage is high:

- [ ] Is M parameter reasonable? (16 is default, lower if needed)
- [ ] Are dimensions minimal? (consider dimension reduction)
- [ ] Is capacity realistic? (don't over-allocate)
- [ ] Is there one index per DB? (multi-index not supported yet)

## Advanced Troubleshooting

### Core Dumps / Panics

If you encounter a panic or segfault:

1. **Enable backtraces:**
   ```bash
   RUST_BACKTRACE=1 cargo run
   ```

2. **Check for usearch issues:**
   - usearch is a C++ library with Rust bindings
   - Memory corruption in usearch can cause crashes
   - Verify embedding dimensions match exactly

3. **Report to maintainers** with:
   - Full backtrace
   - Minimal reproducible example
   - Platform (OS, arch, Rust version)

### Data Corruption

If search results seem corrupted or inconsistent:

1. **Verify data integrity:**
   ```rust
   // Check a few known-good embeddings
   let node = db.get_node(node_id)?;
   let emb = node.get_property("embedding")
       .and_then(|p| p.as_vector())
       .expect("Vector property");

   validate_vector(emb)?;
   println!("Embedding OK: {} dims", emb.len());
   ```

2. **Rebuild index** (future feature - not yet supported):
   ```rust
   // Coming in future release:
   // db.rebuild_vector_index()?;
   ```

3. **Check for concurrent modification bugs**

### Integration with Embeddings Services

Common issues with embedding providers:

1. **API Rate Limits:**
   ```rust
   use std::time::Duration;
   use std::thread;

   for text in texts {
       match get_embedding(text) {
           Ok(emb) => { /* use */ }
           Err(e) if e.to_string().contains("rate limit") => {
               thread::sleep(Duration::from_secs(1));
               // Retry
           }
           Err(e) => return Err(e),
       }
   }
   ```

2. **Token Limits:**
   ```rust
   // Truncate long text before embedding
   let truncated = if text.len() > 8000 {
       &text[..8000]
   } else {
       text
   };
   let embedding = get_embedding(truncated)?;
   ```

3. **Model Changes:**
   ```rust
   // Always specify exact model version
   let embedding = get_openai_embedding(text, "text-embedding-3-small")?;
   // NOT: get_embedding(text) with default model
   ```
