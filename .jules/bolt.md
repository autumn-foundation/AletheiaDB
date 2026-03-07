## 2024-05-22 - Avoid `flat_map` for Binary Serialization
**Learning:** `iterator.flat_map(|x| Vec::new())` is a silent performance killer in serialization hot paths. In `HnswIndex::save`, it allocated a new `Vec` for every single index entry just to flatten 16 bytes, causing massive allocator pressure and slower save times (~20% regression).
**Action:** When serializing fixed-size structures, pre-calculate the total buffer size and write directly to a single `Vec::with_capacity(total_size)`. Avoid intermediate collections or iterators that allocate per-item.

## 2026-06-15 - WAL Serialization Optimization
**Learning:** `PropertyMap::serialized_size()` was O(N) with expensive interner lookups, called per-entry in WAL append for buffer reservation.
**Action:** Cached serialization size in `PropertyMap` during construction (O(1) access), moving cost to mutation time (incremental updates) and eliminating N lookups during WAL append.

## 2026-06-25 - Sort-based vs HashMap-based grouping
**Learning:** Building CSR structures using `HashMap<NodeId, Vec<Entry>>` introduces massive overhead (allocations per node + hashing) compared to sorting the edge list and iterating linearly.
**Action:** For bulk construction of grouped data (like CSR), prefer sorting the flat list by group key (`edges.sort_by_key`) and iterating once, rather than inserting into a grouping HashMap.

## 2026-07-15 - Read-only Interning on Property Map
**Learning:** `PropertyMap::get(key)` called `intern(key)`, which acquires a write lock and allocates memory even for non-existent keys. This turned simple read operations into write operations on the global interner, creating a DoS vector and memory leak for random/non-existent keys.
**Action:** Use `get_id(key)` for read operations (`get`, `contains_key`, `remove`) to check if the key is interned without creating it. Only use `intern()` when inserting new data.

## 2026-10-27 - Identity Hashing for Integer Keys in DashMap
**Learning:** `DashMap<u32, V>` uses `SipHash` by default, which is expensive for simple integer keys that are already unique (like interned IDs). In the `StringInterner::resolve_with` hot path, this hashing overhead was significant.
**Action:** Use `BuildHasherDefault<IdentityHasher>` for maps where keys are already unique integers or IDs to skip the hashing step, improving throughput by ~2.5x.

## 2026-10-27 - Identity Hashing for PropertyMap
**Learning:** `PropertyMap` uses `InternedString` (u32) as keys but was using default `HashMap` hashing (SipHash), incurring ~15-30ns overhead per lookup. Switching to `IdentityHasher` eliminated this, yielding a 3.6x speedup on interned lookups.
**Action:** Audit all usages of `HashMap<InternedString, ...>` or `HashMap<u32, ...>` and replace with `HashMap<..., BuildHasherDefault<IdentityHasher>>` where keys are already high-quality IDs.

## 2026-11-20 - Identity Hashing for WriteBuffer Lookups
**Learning:** `WriteBuffer` used default `HashMap` (SipHash) for `modified_nodes` and `modified_edges` to track dirty state during transactions. Since keys are `NodeId` and `EdgeId` (wrappers around `u64`), hashing overhead was unnecessary and reduced throughput during bulk write operations.
**Action:** Replaced `HashMap` with `FastHashMap` (`HashMap<..., BuildHasherDefault<IdentityHasher>>`) for `modified_nodes` and `modified_edges` in `WriteBuffer`. Using `IdentityHasher` for already-unique integer-like keys eliminates SipHash overhead and improves lookup/insertion speeds during transaction tracking.

**[Removing Intermediate Collections in Iterators]**
**Learning:** Chaining `.collect_all()?` on iterators to convert them to vectors before applying `.into_iter().filter_map(...)` incurs massive unnecessary allocations for large data sets. Also `.collect_all()?.len()` allocates a whole vector just to count elements.
**Action:** When working with iterators, always loop through them directly with `while let Some(item) = iter.next()` to perform transformations and aggregations in a single pass without large intermediate allocations, thus minimizing heap allocations.

## 2026-11-20 - Identity Hashing for Historical Storage
**Learning:** `HistoricalStorage` and `MigrationService` used the default `HashMap` (SipHash) for integer wrapper keys like `NodeId`, `EdgeId`, and `VersionId`. Because these keys are unique internally-assigned high-quality IDs, SipHash incurs unnecessary hashing overhead.
**Action:** Replaced `std::collections::HashMap` with `FastHashMap` (which uses `BuildHasherDefault<IdentityHasher>`) for tracking versions, heads, and stats. Using `IdentityHasher` speeds up tracking and lookups and is idiomatic across AletheiaDB for wrapper IDs.

**Avoid `.map(|r| r.key().clone()).min()`**
**Learning:** `.map(...).min()` allocates a new string (via clone) for *every* element in a `DashMap` (or any iterator of refs) before it computes the minimum. This means `O(N)` heap allocations instead of O(1).
**Action:** Use `.min_by(|a, b| a.key().cmp(b.key()))` first to find the ref to the minimal element, and *then* call `.map(|r| r.key().clone())` to allocate exactly once!

## 2026-11-20 - Avoid `min_by` on DashMap
**Learning:** `DashMap::iter().min_by(...)` and `max_by(...)` hold onto `RefMulti` read guards across loop iterations, which can cause deadlocks if a concurrent writer is waiting for a lock on the same shard.
**Action:** Use `.fold(None, |min, current| ...)` to extract and clone only the necessary minimum/maximum value while immediately dropping the reference guard for each element. This prevents deadlocks and improves concurrency performance.
