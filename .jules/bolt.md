## 2024-05-22 - Avoid `flat_map` for Binary Serialization
**Learning:** `iterator.flat_map(|x| Vec::new())` is a silent performance killer in serialization hot paths. In `HnswIndex::save`, it allocated a new `Vec` for every single index entry just to flatten 16 bytes, causing massive allocator pressure and slower save times (~20% regression).
**Action:** When serializing fixed-size structures, pre-calculate the total buffer size and write directly to a single `Vec::with_capacity(total_size)`. Avoid intermediate collections or iterators that allocate per-item.

## 2024-05-23 - HNSW Search Optimization & Streaming I/O
**Learning:** `sort_by` on already sorted results from `usearch` was unnecessary O(k log k). Streaming I/O with `BufReader`/`BufWriter` avoids massive allocations (file size) during index load/save, critical for large indexes.
**Action:** Trust underlying library guarantees when verified. Use `BufReader`/`BufWriter` for potentially large files instead of `fs::read`/`write`.
