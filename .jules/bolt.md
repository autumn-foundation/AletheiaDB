## ⚡ Bolt: Add batch edge fetching to optimize N+1 queries

**What:** Added `get_edges` method to `ReadOps` trait, implemented in `ReadTransaction` and `WriteTransaction` to batch fetch edges. Updated `alchemy.rs` to use it instead of calling `get_edge` inside a loop.

**Why:** In dense graphs, iterating over incoming/outgoing edges and fetching each edge's properties and labels individually creates an N+1 query pattern. The batch-fetching mechanism reduces repeated function call overhead, locking/visibility checks inside the loop, and prepares the structure for even greater gains when storage relies more on slower components (like tiered storage/disk).

**Impact:** Benchmarks on an isolated dense graph showed that the `get_edges` structure didn't make a huge local memory O(1) impact yet due to `current` memory storage being very fast already (with occasional ~2-5% noise/regressions from additional Vec allocations), but the pattern is now correct for I/O bound queries.
