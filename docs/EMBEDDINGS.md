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
| `embedding-openai` | Compatibility alias for `embeddings` |
| `embedding-huggingface` | Compatibility alias for `embeddings` |
| `embedding-ollama` | Compatibility alias for `embeddings` |
| `embedding-onnx` | Enables `embeddings` plus `embed_anything/ort` |
| `embedding-all` | Enables all compatibility aliases |

New code should generally use `embeddings`. The provider-specific feature names remain so downstream manifests do not break just because we stopped feeding the maintenance monster.

## Basic Flow

```rust,ignore
use aletheiadb::embeddings::{Embedder, EmbeddingResult};
use aletheiadb::{AletheiaDB, PropertyMapBuilder};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENAI_API_KEY")?;
    let embedder =
        Embedder::from_pretrained_cloud("OpenAI", "text-embedding-3-small", Some(api_key))?;

    let documents = [
        "AletheiaDB stores temporal graph facts",
        "Embeddings support semantic retrieval",
    ];

    let results = embedder.embed(&documents, Some(32), None).await?;
    let embeddings = dense_embeddings(results)?;

    let db = AletheiaDB::new()?;
    for (doc, embedding) in documents.iter().zip(embeddings.iter()) {
        db.create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("content", *doc)
                .insert_vector("embedding", embedding)
                .build(),
        )?;
    }

    Ok(())
}

fn dense_embeddings(
    results: Vec<EmbeddingResult>,
) -> Result<Vec<Vec<f32>>, Box<dyn std::error::Error>> {
    results
        .iter()
        .map(EmbeddingResult::to_dense)
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}
```

## Re-Exported Surface

AletheiaDB exposes the upstream crate namespace and the most common types/functions under `aletheiadb::embeddings`:

```rust,ignore
use aletheiadb::embeddings::{
    embed_anything,
    embed_file,
    embed_files_batch,
    embed_query,
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
