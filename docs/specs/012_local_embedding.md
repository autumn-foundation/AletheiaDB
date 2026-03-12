# 🔭 Vantage Spec: Local Embedding Support

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-012 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/vector/` |

## 1. 👤 User Stories

> **As a** Privacy-Conscious Enterprise User,
> **I want to** run embedding models locally on my AletheiaDB nodes,
> **So that** my sensitive data never leaves my infrastructure to reach an external API like OpenAI.

> **As a** Cost-Sensitive Developer,
> **I want to** utilize open-source models like `all-MiniLM-L6-v2` directly within the database,
> **So that** I don't incur per-token API charges for vectorizing millions of existing documents.

## 2. 🧐 The "So What?" (Business Value)

Currently, AletheiaDB's vector search documentation includes a `todo!("Implement local embedding")` for using local models. Users are forced to set up external embedding pipelines or use expensive/privacy-invasive APIs before writing vectors to AletheiaDB.

**The Gap:**
- **Privacy:** External APIs violate strict data sovereignty requirements.
- **Complexity:** Forcing users to manage an external Python/ONNX pipeline just to ingest text adds significant friction to the DX.
- **Cost:** API-based embeddings become prohibitively expensive at scale.

**ROI:**
- **Enterprise Adoption:** Unlocks deals with healthcare, finance, and government sectors that require air-gapped deployments.
- **Developer Experience (DX):** Enables true "batteries-included" vector search. Users can just insert text and AletheiaDB handles the rest.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Local Inference Engine:**
    - The system MUST integrate an ONNX runtime (or similar robust Rust-native ML library) to execute embedding models locally.
    - The integration MUST support common sentence-transformer models (e.g., `all-MiniLM-L6-v2`).

2.  **API Integration:**
    - AletheiaDB's configuration MUST allow specifying a local model file path for a specific vector index.
    - When a node is inserted with a text property tied to a local-embedding vector index, the system MUST automatically generate the embedding and index it.

3.  **Performance:**
    - The system SHOULD pool or reuse inference sessions to minimize overhead during bulk ingestion.

### Metric Definition

- Success = Embedding a 512-token string using `all-MiniLM-L6-v2` locally takes < 50ms per document on standard CPU hardware.

## 4. 🚫 Out of Scope (Phase 1)

-   **GPU Support:** Phase 1 will target CPU inference only to maximize compatibility. GPU acceleration (CUDA/Metal) is deferred to Phase 2.
-   **Model Downloading:** The database will not automatically download models from HuggingFace. Users must provide the model files (e.g., `.onnx` and tokenizer files) locally.
-   **Generative LLMs:** This spec is strictly for *embedding* models, not text generation (LLMs).

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **Local Inference** | Placeholder in docs | Local ML Runtime integrated | Add dependency and engine wrapper |
| **Text Ingestion** | Requires pre-computed vectors | Accepts raw text for auto-embedding | Update property insertion and index logic |
| **Model Config** | N/A | Local path config | Add model path to index configuration |
