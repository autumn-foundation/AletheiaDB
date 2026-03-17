# Echo's DX Audit

## The README Run
- ✅ Basic Graph Operations
- ✅ Time-Travel Queries
- ✅ Vector Search with HNSW
- ✅ Hybrid Queries
- ✅ Semantic Drift Tracking
- ✅ Narrative Generation (Experimental) - `Requires nova feature flag`
- ✅ Index Persistence - `Requires persistence folder clean up, missing index warning displayed on startup`
- ✅ Configuration
- ❌ MCP Server (Claude Integration) - Doesn't compile since it's an external binary? Wait, the README shows `cargo run --bin aletheia-mcp --features mcp-server` so it's a CLI command.
- ❌ Query Language (AQL) - Only gives examples of syntax, no rust code block to run it.
- ✅ Graph Sharding - `Requires sharding-rpc feature flag`
- ❌ Tiered Storage - Errors out with InvalidTimeRange when a previous DB instance already created data. Wait, actually we can run `cargo run --bin test_tier` after clearing out the `aletheiadb` dir! So `std::fs::remove_dir_all("aletheiadb")` is needed to not reuse old state. The README does have a note `⚠️ Note on State` to clear the directory.
- ❌ Transactions - Compile error! "no method named `create_node` found for mutable reference `&mut WriteTransaction`". And "help: trait `WriteOps` which provides `create_node` is implemented but not in scope; perhaps you want to import it". Wait, the `Transactions` example shows:
```rust
use aletheiadb::prelude::*;

// ...
db.write(|tx| {
    let node1 = tx.create_node("Event", PropertyMap::new())?;
    // ...
```
but that throws a compiler error because `WriteOps` is NOT in the `prelude` or not exported correctly, or `prelude::*` doesn't export `WriteOps`. Ah! The README explicitly states: **Note**: The `prelude` module exports `WriteOps` and `ReadOps`, which are required to use methods on the transaction object. BUT my run errored out! Let's check `src/prelude.rs`!
