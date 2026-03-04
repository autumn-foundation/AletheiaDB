# 🗣️ Echo: Getting Started example is broken

## Overview
I went through the README to try and start using this database. It looks cool, but it was incredibly confusing and some things straight up broke when I tried to use them.

## 🤦 The Confusion

1. **Missing `aletheiadb::prelude::*` import**
   In almost all the examples, I had to figure out that I needed to add `use aletheiadb::prelude::*;` to get the basic `properties!` macro and other core stuff working. If I just copy-paste the "Vector Search with HNSW" or "Hybrid Queries" code blocks, they fail to compile because things like `properties!` or `WriteOps` are missing.

2. **Unused Variable Warnings**
   In the Configuration and Sharding examples, the code compiles but immediately gives unused variable warnings (e.g., `let db = AletheiaDB::with_unified_config(config)?` or `let shard = ...`). This makes it feel like the example is incomplete or I did something wrong. Why store it in a variable if the example doesn't use it?

3. **Leftover state / Crash loop**
   When I tried the Configuration example, the code crashed with:
   `Error: Temporal(InvalidTimeRange { start: HybridTimestamp { wallclock: 1772270553325831, logical: 0 }, end: HybridTimestamp { wallclock: 1772270519069799, logical: 0 } })`
   It turns out the database leaves files in the `wal` and `data` directories by default in the current working directory, and running different examples back-to-back causes them to read each other's leftover state and crash! If I am just testing examples, it shouldn't leave state that breaks the next example.

4. **Missing feature flags in Sharding example**
   The Graph Sharding example uses `ShardConfig` and `ShardCoordinator`, but nowhere does it say I need the `sharding-rpc` feature flag! I got an "unresolved import" error until I guessed that I needed to add it to `Cargo.toml`.

## 🕵️ The Reality

- The README assumes the user knows to import the prelude or specific modules like `aletheiadb::properties`, even when they aren't explicitly in the snippet.
- The examples don't clean up their own state, or don't use a temporary directory, leading to nasty crash loops if you run multiple examples in the same folder.
- Feature flags are mentioned in the "Feature Flags" section, but the actual code examples don't remind you to enable them (unlike the Narrative Generation example, which *does* tell you to use the `nova` feature, which was great!).

## 💡 The Fix

- Update the code snippets in the README to include **all** necessary imports, especially `use aletheiadb::prelude::*;`.
- For examples that just show configuration, either use the variable or prefix it with `_` to avoid compiler warnings.
- Mention in the examples (or configure them) to use an in-memory or temporary database if they are just basic demos, or explicitly warn the user that they will create `wal` and `data` directories that might need clearing between runs.
- Add a comment `// REQUIRES FEATURE: sharding-rpc` to the Sharding example, just like the Narrative Generation one.