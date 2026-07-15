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
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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

## MCP embedding tools

The MCP server (feature `mcp-server`) exposes five embedding-backed tools (Issue
#2906) so an LLM/agent can turn text into vectors and run **text** semantic
search without pre-computing embeddings client-side. All five are advertised
unconditionally; they only produce embeddings when the server is **built with the
`embeddings` feature AND configured with a model**
(`AletheiaMcpServer::with_embedder`). Without the feature they return a structured
`FAILED_PRECONDITION` unavailable-feature error; with the feature but no model
configured they return `FAILED_PRECONDITION` ("no embedding model configured").

| Tool | Class | Input | Returns |
|------|-------|-------|---------|
| `embed_query` | read | `{text, model?}` | `{embedding, dim}` |
| `embed_text` | read | `{texts[], model?, max_chunks?}` | `{chunks:[{text, metadata, embedding, dim}], count, truncated}` |
| `semantic_search` | read | `{property_name, query_text, k?, offset?, include_vectors?, model?}` | the exact `find_similar` envelope |
| `create_node_with_embedding` | write | `{label, text, embedding_property, properties?, valid_time?, provenance?}` | the created node |
| `update_node_embedding` | write | `{node_id, text, embedding_property, valid_time?}` | the updated node |

Notes:

- `semantic_search` embeds `query_text` and reuses the **exact** `find_similar`
  path, so its response is byte-identical to `find_similar` (ranked `results` +
  `score`, `temporal` block, vector elision, offset pagination, token budget). It
  refuses with `FAILED_PRECONDITION` when no vector index exists for
  `property_name`, and `INVALID_ARGUMENT` on an embedding-dimension mismatch.
- `embed_text` performs **real chunk expansion**: each input document is split
  into contiguous character windows (`process_chunks` under the hood, not a
  single whole-document `embed_query`), so a long document yields **multiple**
  per-chunk embeddings instead of one silently-truncated vector. Every chunk's
  embedding is aligned to its source chunk text via `EmbedData` (never a
  positional zip); each chunk's `metadata` carries the originating `source_index`
  and `chunk_index`. `max_chunks` is a **hard cap** on the total returned
  embeddings — when it trims the expansion the response sets `truncated: true`;
  `max_chunks: 0` is rejected as `INVALID_ARGUMENT`.
- `update_node_embedding` **preserves** every other property, and does so
  **race-free**: it embeds first (holding no snapshot or lock), then performs the
  read-merge-write inside a **single** write transaction (because `update_node`
  replaces all properties, it re-reads the node from the transaction's own
  snapshot and merges the existing properties before overriding only
  `embedding_property`). A concurrent writer that commits in the window is caught
  by commit-time conflict detection instead of being silently lost.
- Inputs are bounded (per-text byte cap, text-count cap, chunk cap); over-cap
  inputs are rejected with `INVALID_ARGUMENT`.
- The `model` field is reserved/advisory in v1 — the server always uses its
  configured model.

### Local workflow (Codex / Claude, no cloud keys)

Configure the server with a **local** Hugging Face model (no API keys), then let
an agent ingest text and search it end-to-end:

```rust,ignore
use std::sync::Arc;
use aletheiadb::AletheiaDB;
use aletheiadb::embeddings::EmbedderBuilder;
use aletheiadb::mcp::AletheiaMcpServer;

let embedder = Arc::new(
    EmbedderBuilder::new()
        .model_architecture("bert")
        .model_id(Some("sentence-transformers/all-MiniLM-L6-v2"))
        .from_pretrained_hf()?,
);
let db = Arc::new(AletheiaDB::new()?);
let server = AletheiaMcpServer::new(db).with_embedder(embedder);
// server.serve_stdio().await?;  // or serve over HTTP
```

Agent tool sequence:

1. `enable_vector_index` — `{property_name:"embedding", dimensions:384, distance_metric:"cosine"}`.
2. `create_node_with_embedding` — `{label:"Document", text:"Rust is a systems language", embedding_property:"embedding", properties:{title:"Intro"}}` (repeat per document; the embedding is generated and indexed automatically).
3. `semantic_search` — `{property_name:"embedding", query_text:"memory-safe programming", k:5}` returns the closest documents, ranked, in the `find_similar` envelope.

`embed_query` / `embed_text` are also available as standalone text→vector
utilities (e.g. to build a query vector to pass to `find_similar` directly).
Keep credentials out of tool arguments — model configuration is a server concern.

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
