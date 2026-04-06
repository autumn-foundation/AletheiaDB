# 🔭 Vantage Spec: Local Embedding Support

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-015 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Core (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/api/` and `docs/guides/vector-search-integration.md` |

## 1. 👤 User Stories

> **As a** Privacy-Conscious Developer,
> **I want to** run embedding models locally within the database process,
> **So that** sensitive user data never leaves my infrastructure, complying with strict data privacy regulations (like GDPR or HIPAA).

> **As a** Cost-Sensitive Application Owner,
> **I want to** avoid per-token API charges from external providers like OpenAI,
> **So that** my massive bulk-import batch jobs are cost-effective to index.

> **As an** Edge/Offline User,
> **I want to** embed documents and search the graph without internet access,
> **So that** my local desktop or on-premise application remains fully functional in disconnected environments.

## 2. 🧐 The "So What?" (Business Value)

Currently, AletheiaDB users who want vector embeddings must either use a paid external API (like OpenAI) or manually implement their own local ML integration before inserting data into the database. This is a massive friction point, explicitly left as a `todo!("Implement local embedding")` in the documentation.

**The Gap:**
- **Developer Experience (DX):** Users are forced to learn ML frameworks (ONNX, Candle, HuggingFace), manage tokenizers, and coordinate the ML inference lifecycle with the DB transaction lifecycle.
- **Privacy:** Many enterprise users cannot send their knowledge graphs to external APIs.

**ROI:**
- **Broader Adoption:** Eliminates the barrier to entry for local, privacy-first AI development.
- **Seamless DX:** Transforms a complex external integration into a single configuration flag (`OpenAIConfig` vs `LocalModelConfig`).
- **Cost Savings:** Enables entirely free (compute-only) text-to-vector workflows.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1. **Native Local Model Integration:**
   - The system MUST provide an integrated local embedding provider using a Rust-native ML backend (e.g., `ort` or `candle-core`).
   - The user MUST be able to download and specify common models (e.g., `all-MiniLM-L6-v2`) via configuration.
2. **Transparent Auto-Embedding:**
   - Similar to external APIs, the system MUST automatically intercept properties configured for embedding, generate the local vector, and store it seamlessly during the transaction.
3. **Cross-Platform Compatibility:**
   - The local embedding feature MUST compile and run on major architectures (x86_64, aarch64) without forcing users to install complex C++ toolchains manually.

### Non-Functional Requirements

- **Performance:** Local embedding inference MUST NOT block the main transaction thread. It should process document batches efficiently.
- **Metric Definition:** Success = A batch of 1,000 short documents embeds and indexes in < 2 seconds on a standard M-series Mac or equivalent CPU.

## 4. 🚫 Out of Scope (Phase 1)

- **GPU Acceleration / CUDA Management:** Complex GPU compilation and drivers are out of scope. We will rely on CPU inference via ONNX/Candle for Phase 1.
- **Auto-Downloading Models:** In Phase 1, the user must provide the path to the model weights. The database will not act as a HuggingFace client.
- **Multi-Modal Embeddings:** Only text embeddings are supported. Image/Audio embeddings are deferred.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **Local ML Inference** | Left as user exercise (`todo!()`) | Natively integrated | Add `ort` or `candle` to dependencies |
| **Local Config** | Only OpenAI configs exist | `LocalModelConfig` struct | Create configuration abstraction |
| **Documentation**| Mentions `sentence-transformers` | Working code examples | Update `vector-search-integration.md` |
