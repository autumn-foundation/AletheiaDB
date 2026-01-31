## 2024-05-22 - Avoid `flat_map` for Binary Serialization
**Learning:** `iterator.flat_map(|x| Vec::new())` is a silent performance killer in serialization hot paths. In `HnswIndex::save`, it allocated a new `Vec` for every single index entry just to flatten 16 bytes, causing massive allocator pressure and slower save times (~20% regression).
**Action:** When serializing fixed-size structures, pre-calculate the total buffer size and write directly to a single `Vec::with_capacity(total_size)`. Avoid intermediate collections or iterators that allocate per-item.

## 2026-06-15 - WAL Serialization Optimization
**Learning:** `PropertyMap::serialized_size()` was O(N) with expensive interner lookups, called per-entry in WAL append for buffer reservation.
**Action:** Cached serialization size in `PropertyMap` during construction (O(1) access), moving cost to mutation time (incremental updates) and eliminating N lookups during WAL append.

## 2026-06-25 - Sort-based vs HashMap-based grouping
**Learning:** Building CSR structures using `HashMap<NodeId, Vec<Entry>>` introduces massive overhead (allocations per node + hashing) compared to sorting the edge list and iterating linearly.
**Action:** For bulk construction of grouped data (like CSR), prefer sorting the flat list by group key (`edges.sort_by_key`) and iterating once, rather than inserting into a grouping HashMap.
