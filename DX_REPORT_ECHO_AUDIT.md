# Echo's DX Audit Report 🗣️

**Date:** 2025-05-24
**Auditor:** Echo (The Impatient User)

## Summary

I tried to run the examples from the `README.md` by copy-pasting them into a fresh project. Most examples worked great, but I hit a major wall with the **Tiered Storage** example.

## 🟢 The Good

- **Basic Graph Operations**: Copy-paste worked immediately.
- **Time-Travel Queries**: Worked seamlessly.
- **Vector Search**: Worked well. `db.create_node` convenience method is a nice touch.
- **Sharding**: Compiled fine (with feature flag).

## 🔴 The Bad

### 1. Tiered Storage Example is Broken/Misleading

The example code in `README.md` shows how to manually create a `HistoricalStorage` instance:

```rust
// Configure historical storage
let mut historical = HistoricalStorage::new();
historical.set_tiered_storage(Arc::new(tiered));
```

**The Problem:** There is no shown way to pass this `historical` object into `AletheiaDB`. The `AletheiaDB::new()` method creates its own internal storage. The user is left with a configured storage component that is detached from the database they want to use.

**The Fix:** Update the example to use the "Unified Configuration" pattern which is the correct way to enable tiered storage in `AletheiaDB`.

### 2. Narrative Generation Feature Flag

The example says "Ensure you have features = ["nova"] enabled", but if I just copy the code block (as users do), I get a compile error.

**The Fix:** While the comment is there, explicitly showing the run command `cargo run ... --features nova` in the code block comments or right above it would be even better. (The README does have this command above the code block, so this is a minor nitpick).

## Action Items

- [ ] Update `README.md` Tiered Storage example to use `AletheiaDBConfig`.
- [ ] (Optional) improve Narrative Generation example comments.

## Verified Fix

I verified that the following code works for Tiered Storage:

```rust
use aletheiadb::{AletheiaDB, config::AletheiaDBConfig};
use aletheiadb::config::HistoricalConfigBuilder;
use std::time::Duration;

// Configure cold storage via the unified config builder
let config = AletheiaDBConfig::builder()
    .historical(
        HistoricalConfigBuilder::new()
            .enable_cold_storage(true)
            .cold_storage_path("data/cold.redb")
            .migration_age_threshold(Duration::from_secs(3600))
            .max_hot_versions(1000)
            .build(),
    )
    .build();

let db = AletheiaDB::with_unified_config(config)?;
```
