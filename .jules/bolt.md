**Avoid intermediate Vec allocations when parsing queries**
**Learning:** `cleaned.split_whitespace().collect::<Vec<_>>().join(" ");` creates an unnecessary `Vec` allocation on the heap, which isn't necessary for simple string transformations.
**Action:** Use a pre-allocated string with `String::with_capacity(cleaned.len())` and a loop over the word iterator to concatenate spaces and words directly.

**Refactor BFS `TraversalIterator` to avoid intermediate Vec allocations**
**Learning:** During graph traversals (like BFS), returning a `Vec` of neighbors from a helper method on each node evaluation can accumulate significant heap allocation overhead over time.
**Action:** Replace `get_neighbors() -> Vec<T>` with a higher-order function like `process_neighbors<F>(mut f: F)` that lazily yields each neighbor directly to the required collection (like a `VecDeque` frontier), achieving a zero-cost abstraction without fighting the borrow checker (by defining it as an associated function instead of a method taking `&self`).
