# 🗣️ Echo: Index Persistence examples crash on shutdown/restart with sparse vectors

🤦 **The Confusion:**
I copy-pasted the "Index Persistence" and "Semantic Drift Tracking" examples from `README.md` to see how the database handles vectors and persistence. But when I run it, the database crashes with a terrifying panic: `Failed to persist temporal index: Storage error: Persistence error: Failed to convert node version 1: Serialization error: Cannot persist NodeVersion 1: VectorDelta::Sparse found for property key InternedString("embedding"). Call PropertyDelta::materialize_vector_deltas() before persistence to prevent data loss.`

What the heck is a `materialize_vector_deltas()`? I just used the `AletheiaDB::new()` and `db.vector_index()` API exactly like the docs showed!

🕵️ **The Reality:**
It looks like when the database tries to save its state or shut down, it chokes on some "sparse vector deltas" that got created by the temporal vector tracking. The example works fine until it hits the disk, and then boom.

💡 **The Fix:**
The database should handle its own "materialization" under the hood before it saves to disk. Don't make the user call random internal methods like `materialize_vector_deltas()` just to shut down the database without crashing! Fix the persistence layer to automatically materialize these vectors so the examples actually run without exploding on exit.
