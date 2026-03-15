# 🗣️ Echo: Getting Started example is broken

## Description

🤦 **The Confusion:** Tried to run the examples from the README.md and ran into several confusing issues during the "README Run".

### 1. The "Basic Graph Operations" Warning (Minor)
**Scenario:** Copied the first example.
**Result:** `warning: unused variable: alice`
**Why it hurts:** New users hate yellow text. It makes me think I did something wrong.
**The Reality:** The `alice` variable is assigned but never used or printed.
**💡 The Fix:** Add `let _ = alice;` or print it: `println!("{:?}", alice);`.

### 2. The "Time-Travel" Type Mismatch (Major)
**Scenario:** Tried "Time-Travel Queries".
**Code:** `if let Some(old_alice) = historical_alice { ... }`
**Result:** `error[E0308]: mismatched types. expected Node, found Option<_>`
**The Reality:** `db.get_node_at_time` returns `Result<Node>`, not `Result<Option<Node>>`.
**💡 The Fix:** Remove the `if let Some(...)` unwrapping, or update the API to return Option (if that was the intent).

### 3. The "Tiered Storage" Box vs Arc (Major)
**Scenario:** Tried "Tiered Storage".
**Code:** `TieredStorage::with_default_config(Box::new(cold))`
**Result:** `error[E0308]: mismatched types. expected Arc<RedbColdStorage>, found Box<RedbColdStorage>`
**The Reality:** The API expects `Arc`, but the docs say `Box`. Or better yet, there is no shown way to pass this `historical` object into `AletheiaDB`. The user is left with a configured storage component that is detached from the database they want to use.
**💡 The Fix:** Update the example to use the "Unified Configuration" pattern which is the correct way to enable tiered storage in `AletheiaDB` (using `AletheiaDBConfig::builder()`).

### 4. The "Narrative" Context Gap (Minor)
**Scenario:** Tried "Narrative Generation".
**Result:** The snippet is partial (requires `db` and `node_id`).
**The Reality:** Users need to setup `db` and a `node_id` for it to run.
**💡 The Fix:** While `story_demo.rs` exists and works, the README snippet should probably be self-contained or explicitly reference the setup.

### 5. Verbose Property Construction (Minor)
**Scenario:** Looking through examples and the README.
**Result:** Code like `PropertyMapBuilder::new().insert(...).build()` is used extensively.
**The Reality:** This is verbose and repetitive. The `properties!` macro exists and provides a much cleaner syntax.
**💡 The Fix:** Update documentation and examples to prioritize the `properties!` macro.
