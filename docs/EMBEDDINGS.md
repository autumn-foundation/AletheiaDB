# Embedding Generation

AletheiaDB stores, indexes, and queries vector properties. It no longer maintains provider-specific embedding clients. Enable the `embeddings` feature to use the re-exported [`embed_anything`](https://crates.io/crates/embed_anything) API, generate embeddings in application code, and store the resulting dense vectors with `PropertyMapBuilder::insert_vector()`.

## Ownership Boundary

AletheiaDB owns:

- vector property storage
- vector validation at database/index boundaries
- HNSW indexes
- temporal vector history
- semantic drift queries
- hybrid graph/vector query behavior

`embed_anything` owns:

- cloud and local embedding model construction
- provider API details
- file, web, image, and media loading
- chunking and splitting
- ONNX and accelerator integration
- model backend maintenance

That split keeps the database out of the provider treadmill. The graveyard of half-finished embedding clients can rest.

## Feature Flags

```toml
[dependencies]
aletheiadb = { version = "0.1", features = ["embeddings"] }
```

| Feature | Purpose |
|---------|---------|
| `embeddings` | Enables the `embed_anything` dependency and AletheiaDB re-exports |
| `embeddings-onnx` | Enables `embeddings` plus `embed_anything/ort` for ONNX backends |

New code should use `embeddings`. Use `embeddings-onnx` only when the application needs `embed_anything`'s ONNX runtime path.

## Basic Flow

Use `EmbedData`-based APIs when storing generated embeddings. `EmbedData` keeps the produced vector attached to the text chunk and metadata that generated it, which matters once file parsing or splitting produces more chunks than original input documents.

```rust,ignore
use aletheiadb::embeddings::{embed_data_to_dense_iter, embed_query, Embedder};
use aletheiadb::{AletheiaDB, PropertyMapBuilder};

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENAI_API_KEY")?;
    let embedder =
        Embedder::from_pretrained_cloud("OpenAI", "text-embedding-3-small", Some(api_key))?;

    let documents = [
        "AletheiaDB stores temporal graph facts",
        "Embeddings support semantic retrieval",
    ];

    let data = embed_query(&documents, &embedder, None).await?;

    let db = AletheiaDB::new()?;
    for item in embed_data_to_dense_iter(data, None) {
        let item = item?;
        if let Some(content) = item.text {
            db.create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert("content", content)
                    .insert_vector("embedding", &item.embedding)
                    .build(),
            )?;
        }
    }

    Ok(())
}
```

Avoid converting to `Vec<Vec<f32>>` and zipping back to the original documents for file, web, or chunked text workflows. Chunking can increase result cardinality; zipping silently drops extra chunks or associates vectors with the wrong source text.

## Chunked Input

For already-chunked text, use the re-exported `process_chunks` API so metadata stays attached through embedding generation:

```rust,ignore
use std::{collections::HashMap, sync::Arc};

use aletheiadb::embeddings::{embed_data_to_dense_iter, process_chunks, Embedder};
use aletheiadb::{AletheiaDB, PropertyMapBuilder};

# async fn example(embedder: Embedder) -> Result<(), Box<dyn std::error::Error>> {
let chunks = vec![
    "AletheiaDB stores temporal graph facts".to_string(),
    "Embeddings support semantic retrieval".to_string(),
];
let metadata = chunks
    .iter()
    .enumerate()
    .map(|(index, _)| {
        let mut metadata = HashMap::new();
        metadata.insert("document_id".to_string(), "doc-1".to_string());
        metadata.insert("chunk_index".to_string(), index.to_string());
        Some(metadata)
    })
    .collect::<Vec<_>>();

let embedder = Arc::new(embedder);
let data = process_chunks(&chunks, &metadata, &embedder, Some(32), None).await?;

let db = AletheiaDB::new()?;
for item in embed_data_to_dense_iter(data.iter().cloned(), Some(10_000)) {
    let item = item?;
    if let Some(content) = item.text {
        db.create_node(
            "DocumentChunk",
            PropertyMapBuilder::new()
                .insert("content", content)
                .insert_vector("embedding", &item.embedding)
                .build(),
        )?;
    }
}
# Ok(())
# }
```

The optional limit on `embed_data_to_dense_iter` and `to_dense_iter` bounds conversion work for large embedding batches.

## Re-Exported Surface

AletheiaDB exposes the upstream crate namespace and the most common types/functions under `aletheiadb::embeddings`:

```rust,ignore
use aletheiadb::embeddings::{
    embed_anything,
    embed_data_to_dense_iter,
    embed_file,
    embed_files_batch,
    embed_query,
    process_chunks,
    to_dense_iter,
    DenseEmbedData,
    DenseEmbeddingError,
    EmbedData,
    Embedder,
    EmbedderBuilder,
    EmbeddingResult,
    ImageEmbedConfig,
    SplittingStrategy,
    TextEmbedConfig,
};
```

If you need a newer or more specialized upstream API, use the re-exported crate namespace:

```rust,ignore
use aletheiadb::embeddings::embed_anything;
```

## Database Contract

Vector storage has not changed. The database expects dense `f32` slices and validates them at the same boundaries as before:

```rust,ignore
PropertyMapBuilder::new()
    .insert("content", content)
    .insert_vector("embedding", &embedding)
    .build();
```

All existing graph/vector/temporal APIs continue to operate on stored vectors. Embedding generation is now an application concern, not a database subsystem.

## Migration

Removed AletheiaDB-owned APIs:

- `EmbeddingService`
- `EmbeddingProvider`
- `EmbeddingError`
- `providers::openai::*`
- `providers::huggingface::*`
- `providers::ollama::*`
- `providers::onnx::*`

Replace them with upstream `embed_anything` construction and conversion. If an application needs a stable provider abstraction, put that adapter in the application layer where provider policy actually belongs.

## See Also

- [ADR 0016: Delegate Embedding Generation to embed_anything](./adr/0016-embedding-providers.md)
- [embed_anything crate](https://crates.io/crates/embed_anything)
- [embed_anything docs](https://docs.rs/embed_anything)
- [Vector Search Design](./VECTOR_SEARCH_DESIGN.md)
