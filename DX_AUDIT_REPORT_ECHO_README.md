# 🗣️ Echo: Getting Started Examples Audit

## 🔎 EXPERIENCE - The Walkthrough
**Scenario:** "I am a new user trying to run the examples from the `README.md` file to learn how to use AletheiaDB."
**Action:** Literally copy-paste the code blocks from the "Usage Examples" section in `README.md` into new `.rs` files (simulating a fresh `main.rs`) and try to `cargo run` them.

## 🚧 STUMBLE - The Friction Points
During the walkthrough, several examples failed to compile or run out of the box.

1.  **AQL Query Failure:** The `Query Language (AQL)` example compiles but fails at runtime with:
    `Error: Query(InvalidParameter { parameter: "timestamp", reason: "Invalid timestamp '2024-01-15T10...`
    The AQL string uses an ISO 8601 date (`AS OF '2024-01-15T10:00:00Z'`), but the parser expects microseconds since the epoch.

2.  **State Contamination across Examples:** Running the `dx_vector` example successfully indexes an embedding and writes to the default persistence directory (`aletheiadb/`). However, subsequently running the `Index Persistence`, `Configuration`, or `Tiered Storage` examples (which also use the default or a similar persistence setup) causes a runtime warning/error:
    `Failed to persist temporal index: Storage error: Persistence error... VectorDelta::Sparse found for property key InternedString("embedding"). Call PropertyDelta::materialize_vector_deltas() before persistence to prevent data loss.`
    Even though the README has a "Note on State", new users often just run things sequentially and will hit this scary error.

3.  **Experimental Features Gate:** The `Narrative Generation (Experimental)` example fails to compile with:
    `error[E0432]: unresolved import aletheiadb::experimental::temporal_narrative`
    The code snippet says `// ⚠️ REQUIRES FEATURE: nova`, but if a user just copies the Rust code without updating their `Cargo.toml`, they get a confusing compiler error.

4.  **Observability Feature Gate:** The `Production Observability (Optional)` example fails to compile with:
    `error[E0432]: unresolved import aletheiadb::observability`
    Similar to the Nova feature, if the user doesn't update `Cargo.toml`, it fails.

5.  **Embeddings Feature Gate:** The `Embedding Generation (Optional)` example fails to compile with:
    `error[E0433]: failed to resolve: could not find embeddings in aletheiadb`
    This requires multiple features (`embeddings`, `embedding-openai`) and also `tokio`.

6.  **Compiler Warnings:** The `Transactions` and `AQL` examples trigger `unused variable` warnings (e.g., `let result = ...` and `let results = ...` are never used).

## 📢 REPORT - The Complaint
- 🤦 **The Confusion:** "I copied the basic examples from the README and half of them didn't work! Some wouldn't compile because of missing features, one crashed with an 'Invalid timestamp' error, and my database got corrupted with a `VectorDelta::Sparse` error just by running the examples in order."
- 🕵️ **The Reality:** The examples require specific feature flags in `Cargo.toml` that aren't enabled by default. The AQL example is factually incorrect (uses ISO 8601 instead of microseconds). The database state persists between runs and the vector example leaves it in a state that crashes subsequent examples during recovery.
- 💡 **The Fix:**
    - Fix the AQL example to use a valid timestamp format (microseconds).
    - Fix the `unused variable` warnings by prefixing variables with `_` or printing them.
    - For examples requiring features, consider making them compile-time gated or adding huge, unmissable warnings in the code blocks themselves.
    - For the state contamination issue, either configure the examples to use an in-memory or temporary directory by default, or provide a clear `cleanup()` function in the examples.

## 🧪 VERIFY - The idiot proofing
If the AQL query example is updated to use microseconds, a user should be able to copy-paste it and run it without an `InvalidParameter` crash. If the examples are updated to use temporary directories, users should be able to run all examples sequentially without hitting persistence errors.