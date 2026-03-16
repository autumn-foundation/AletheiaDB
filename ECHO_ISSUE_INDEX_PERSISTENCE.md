# 🗣️ Echo: Index Persistence example crashes with VectorDelta::Sparse error

## 🤦 The Confusion
I am a new user trying to run the examples in `README.md`. I created a fresh `main.rs` and copy-pasted the **Vector Search with HNSW** example. It ran perfectly!
Then I wanted to see how fast cold starts are, so I copy-pasted the next example, **Index Persistence (Fast Cold Starts)**, into `main.rs` and ran it.

It immediately crashed with a terrifying panic:
```text
Failed to persist temporal index: Storage error: Persistence error: Failed to convert node version 1: Serialization error: Cannot persist NodeVersion 1: VectorDelta::Sparse found for property key InternedString("embedding"). Call PropertyDelta::materialize_vector_deltas() before persistence to prevent data loss.
```

What is a `VectorDelta::Sparse`? What is `materialize_vector_deltas()`? I just want to load the database! The example said "Indexes automatically persist in background" and "2-5s cold start". It didn't mention anything about sparse deltas!

## 🕵️ The Reality
AletheiaDB persists data to disk in `./aletheiadb` by default. When I ran the **Vector Search** example, it created a database and stored nodes with vector embeddings (`embedding` property).
Then, when I ran the **Index Persistence** example in the exact same directory, it tried to load the existing database that I had just created, which had unmaterialized vector deltas pending. The persistence background task immediately tripped over this and crashed because it requires `PropertyDelta::materialize_vector_deltas()` to be called.

## 💡 The Fix
1. Add a massive, bold warning in the README right before the examples:
   > ⚠️ **Note on State:** AletheiaDB persists data to disk by default (in `./aletheiadb`). If you run multiple examples in the same folder, they will conflict! Either clear the `aletheiadb` directory between runs, or configure a temporary directory.
2. The `AletheiaDBConfig` builder in the Index Persistence example should explicitly use a different data directory so it doesn't collide with the default `./aletheiadb` directory. Wait, it currently uses `data_dir: "data/my-database".into()`, but the previous example created the `aletheiadb` default directory. Actually, when `AletheiaDB::with_unified_config` runs, it may still be tripping over something, or maybe the default data directory is `aletheiadb` and the index persistence uses a different one?
Wait, the vector index writes to the WAL, and the Index persistence example replayed the WAL! "Replaying 11 WAL entries from LSN 1... Failed to persist temporal index...". The WAL replay mechanism crashes if there are unmaterialized vector deltas!
3. The real fix: The database should automatically materialize vector deltas during WAL replay or persistence, instead of making the user do it (or at least provide a helpful error message). The user shouldn't need to know about internal engine mechanics like `materialize_vector_deltas` just to load a database!