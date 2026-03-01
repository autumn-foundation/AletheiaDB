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

**[IdentityHasher for Integer Keys]**
**Learning:** Using `HashMap` with default hasher for small integer-wrapper keys like `NodeId` or `EdgeId` causes unnecessary hashing overhead. `FastHashMap` (using `IdentityHasher`) should be used to avoid this overhead and improve lookup/insertion speeds.
**Action:** Replace `HashMap` with `FastHashMap` for integer keys when hashing overhead is a bottleneck, especially in hot paths like `WriteBuffer` tracking modified entities.
