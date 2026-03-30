# 🔭 Vantage Spec: Vector Dimension Reduction

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-014 |
| **Status** | 📝 Draft |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/core/vector/` (Proposed) |

## 1. 👤 User Stories

> **As an** AI Application Developer,
> **I want to** automatically reduce the dimensionality of my high-dimensional embeddings (e.g., from 3072 to 384 dimensions),
> **So that** I can dramatically reduce memory usage and increase query speed while maintaining acceptable recall.

> **As a** DevOps Engineer,
> **I want to** fit my entire vector index in RAM on smaller, cheaper cloud instances,
> **So that** I can lower my infrastructure costs without sacrificing the real-time performance of my semantic search.

> **As a** Data Scientist,
> **I want to** apply Principal Component Analysis (PCA) to my embeddings during the ingestion pipeline natively,
> **So that** I don't have to manage an external python script to preprocess data before inserting it into AletheiaDB.

## 2. 🧐 The "So What?" (Business Value)

Modern embedding models (like OpenAI's `text-embedding-3-large`) produce extremely high-dimensional vectors (e.g., 3072 dimensions). While this captures a lot of semantic nuance, it also explodes memory requirements.
Currently, users are forced to store the full 3072-dimensional vectors. In `docs/guides/vector-search-performance.md`, we suggest dimension reduction but leave it as a `todo!("Implement dimension reduction")` for the user to implement themselves.

**The Gap:**
- **Cost**: A 3072-dim vector takes 8x more memory than a 384-dim vector. For 1 million vectors, that's the difference between 1.5 GB and 12 GB of RAM just for the raw data.
- **Performance**: Distance calculations (Cosine, L2) scale linearly with dimensionality. High-dim vectors mean slower queries and higher CPU usage.
- **Developer Experience**: Forcing users to handle PCA outside the database adds friction to the ingestion pipeline.

**ROI:**
- **Operational Cost**: Enables running large-scale semantic search on significantly cheaper hardware.
- **Performance**: Increases throughput and lowers latency for vector searches.
- **Developer Experience**: AletheiaDB handles the complexity, allowing users to just use the best models available without worrying about the infrastructure tax.

## 3. ✅ Acceptance Criteria

### Functional Requirements
1.  **Reduction API**:
    - Must provide a configuration option when creating a vector index to specify a `target_dimensions` value.
    - If `target_dimensions` is less than the raw input dimensions, the system must automatically project the vector to the lower dimensional space during ingestion.
    - Queries must also automatically project the query vector into the same space before searching the index.

2.  **Reduction Methods**:
    - Must support **PCA (Principal Component Analysis)** as the primary deterministic reduction method.
    - Must allow fitting a PCA model on a representative batch of data before enabling the index, or allow loading a pre-trained projection matrix.

3.  **Matryoshka Representation Learning (MRL) Support**:
    - Must natively support truncation for MRL-trained models (like `text-embedding-3-small/large`). If the model natively supports truncation, the system should simply take the first `N` dimensions instead of requiring a full PCA.

### Non-Functional Requirements
-   **Performance**: The projection overhead during ingestion and querying must be < 1ms per vector.
-   **Accuracy Metric**: Dimension reduction from 1536 to 384 should not degrade recall@10 by more than 2-3% on standard benchmarks.

## 4. 🚫 Out of Scope (Phase 1)

-   **Complex Autoencoders**: Training deep neural networks or complex non-linear autoencoders natively within AletheiaDB. We will stick to linear projections (PCA) and simple truncation for Phase 1.
-   **Dynamic Re-projection**: Changing the `target_dimensions` of an existing, populated index without a full rebuild.
-   **Product Quantization (PQ)**: This is a separate compression technique (compressing vector values into smaller bit representations, rather than dropping dimensions). This spec focuses strictly on dimension reduction.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **Ingestion** | Stores raw vectors only | Applies projection if configured | Update `create_node` vector handling |
| **Querying** | Uses raw vectors only | Applies projection to query vector | Update `search_vectors` logic |
| **Configuration**| `HnswConfig` only | `ReductionConfig` | Extend Vector Index schema |
| **Documentation**| Left as `todo!()` in guides | Natively supported | Update performance guide with examples |
