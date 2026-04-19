# 🗣️ Echo DX Report: The "README Run" Audit

**Auditor:** Echo (Voice of the User)
**Subject:** `README.md` Copy-Paste Verification

## 🔍 The Walkthrough

I performed the "README Run": literally copy-pasting code blocks from `README.md` into a fresh Rust file to see if they compile and run.

### 🚧 The Friction Points

#### 1. The "Basic Graph Operations" Warning (Minor)
**Scenario:** Copied the first example.
**Result:** `warning: unused variable: alice`
**Why it hurts:** New users hate yellow text. It makes me think I did something wrong.
**Fix:** Add `let _ = alice;` or print it: `println!("{:?}", alice);`.

#### 2. The "Time-Travel" Type Mismatch (Major)
**Scenario:** Tried "Time-Travel Queries".
**Code:** `if let Some(old_alice) = historical_alice { ... }`
**Result:** `error[E0308]: mismatched types. expected Node, found Option<_>`
**The Reality:** `db.get_node_at_time` returns `Result<Node>`, not `Result<Option<Node>>`.
**Fix:** Remove the `if let Some(...)` unwrapping, or update the API to return Option (if that was the intent).

#### 3. The "Vector Search" Mystery Variable (Major)
**Scenario:** Tried "Vector Search with HNSW".
**Code:** `.insert_vector("embedding", &embedding)`
**Result:** `error[E0425]: cannot find value 'embedding' in this scope`
**The Reality:** The example assumes I have an embedding vector ready, but doesn't show how to create one.
**Fix:** Add `let embedding = vec![0.0f32; 384];` before the usage.

#### 4. The "Tiered Storage" Box vs Arc (Major)
**Scenario:** Tried "Tiered Storage".
**Code:** `TieredStorage::with_default_config(Box::new(cold))`
**Result:** `error[E0308]: mismatched types. expected Arc<RedbColdStorage>, found Box<RedbColdStorage>`
**The Reality:** The API expects `Arc`, but the docs say `Box`.
**Fix:** Change `Box::new(cold)` to `Arc::new(cold)`.

#### 5. The "Narrative" Context Gap (Minor)
**Scenario:** Tried "Narrative Generation".
**Result:** The snippet is partial (requires `db` and `node_id`).
**Fix:** While `story_demo.rs` exists and works, the README snippet should probably be self-contained or explicitly reference the setup.

## 🏁 Conclusion

3 out of 5 checked examples **failed to compile**.
The documentation seems to drift from the actual API implementation (`Node` vs `Option<Node>`, `Box` vs `Arc`).

**Echo's Verdict:** The README is currently a minefield for copy-pasters.

Signed,
**Echo** 🗣️
