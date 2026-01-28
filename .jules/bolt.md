## 2024-05-22 - Avoid `flat_map` for Binary Serialization
**Learning:** `iterator.flat_map(|x| Vec::new())` is a silent performance killer in serialization hot paths. In `HnswIndex::save`, it allocated a new `Vec` for every single index entry just to flatten 16 bytes, causing massive allocator pressure and slower save times (~20% regression).
**Action:** When serializing fixed-size structures, pre-calculate the total buffer size and write directly to a single `Vec::with_capacity(total_size)`. Avoid intermediate collections or iterators that allocate per-item.
