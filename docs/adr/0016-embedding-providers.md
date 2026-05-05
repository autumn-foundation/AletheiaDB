# ADR 0016: Delegate Embedding Generation to embed_anything

**Status**: Supersedes the previous custom provider architecture
**Date**: 2026-05-04
**Authors**: Mark M., Codex
**Related**: [EMBEDDINGS.md](../EMBEDDINGS.md), [VECTOR_SEARCH_DESIGN.md](../VECTOR_SEARCH_DESIGN.md)

## Context

AletheiaDB stores vector embeddings as first-class properties, indexes them with HNSW, tracks temporal vector history, and supports hybrid graph/vector queries. Those are database responsibilities.

The previous embedding subsystem also tried to own provider clients for OpenAI, Hugging Face, Ollama, and ONNX. That created a second product inside the database crate:

- provider-specific config types
- HTTP client behavior
- model/provider selection policy
- local runtime/tokenizer details
- a placeholder ONNX implementation
- extra dependency and feature maintenance

Embedding providers evolve quickly. Keeping those clients in AletheiaDB is a maintenance burden for a graph database. Spooky action at a distance is for physics, not dependency ownership.

## Decision

AletheiaDB will not maintain custom embedding providers. The optional `embeddings` feature now re-exports [`embed_anything`](https://crates.io/crates/embed_anything) and common upstream types/functions.

AletheiaDB remains responsible for:

- storing dense vectors as properties
- validating vectors at database/index boundaries
- indexing vectors with HNSW
- temporal vector history and semantic drift queries
- hybrid graph/vector query behavior

`embed_anything` is responsible for:

- provider/model construction
- local/cloud embedding generation
- file/web/media loading
- chunking
- ONNX/GPU/model backend details
- upstream provider API changes

## Feature Flags

```toml
embeddings = ["dep:tokio", "dep:embed_anything"]
embedding-openai = ["embeddings"]
embedding-huggingface = ["embeddings"]
embedding-ollama = ["embeddings"]
embedding-onnx = ["embeddings", "embed_anything/ort"]
embedding-all = [
    "embedding-openai",
    "embedding-huggingface",
    "embedding-onnx",
    "embedding-ollama",
]
```

The provider-specific feature names are retained as compatibility aliases. New code should use `embeddings` unless it needs `embedding-onnx`.

## Consequences

Positive:

- Less custom code in AletheiaDB.
- No unfinished local ONNX path pretending to be useful.
- Faster provider/model coverage through upstream.
- Clearer separation between embedding generation and database indexing.
- Existing vector storage/search APIs remain unchanged.

Negative:

- Public provider structs are removed.
- The custom service wrapper and provider trait are removed.
- Downstream code must migrate to `embed_anything` types or keep its own application-layer adapter.
- The `embeddings` feature now pulls in `embed_anything`'s dependency graph when enabled.

## Migration

Old code configured provider-specific AletheiaDB structs and called a database-owned service wrapper. New code configures `embed_anything` directly.

New:

```rust,ignore
use aletheiadb::embeddings::{Embedder, EmbeddingResult};

let api_key = std::env::var("OPENAI_API_KEY")?;
let embedder =
    Embedder::from_pretrained_cloud("OpenAI", "text-embedding-3-small", Some(api_key))?;
let results = embedder.embed(&["hello"], Some(1), None).await?;
let embedding = results
    .first()
    .ok_or("empty embedding result")?
    .to_dense()?;
```

Then store the dense vector exactly as before:

```rust,ignore
PropertyMapBuilder::new()
    .insert("content", "hello")
    .insert_vector("embedding", &embedding)
    .build();
```

## Verification

The public contract is covered by a compile-time integration test that imports `aletheiadb::embeddings::{embed_anything, EmbedData, EmbeddingResult}` and exercises dense vector conversion through the re-exported upstream types.
