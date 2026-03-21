**Avoid intermediate Vec allocations when parsing queries**
**Learning:** `cleaned.split_whitespace().collect::<Vec<_>>().join(" ");` creates an unnecessary `Vec` allocation on the heap, which isn't necessary for simple string transformations.
**Action:** Use a pre-allocated string with `String::with_capacity(cleaned.len())` and a loop over the word iterator to concatenate spaces and words directly.

**Pre-allocated vectors in sharding scatter logic**
**Learning:** Found multiple vectors (`results`, `failures`, `aggregated`) in the critical path (`execute` and `aggregate_results`) being created without capacity, resulting in potentially multiple heap reallocations. `results` and `failures` size depends directly on `target_shards.len()`. The `aggregated` vector size depends on the sizes of all result data fragments.
**Action:** Always pre-calculate the required vector capacity when creating `Vec`s in a collection loop, using methods like `.sum()` on `.map(|x| x.len())` for collection concatenation.

**Pre-allocated vector sizes based on strategy**
**Learning:** When pre-allocating vectors based on multiple inputs, be mindful of the aggregation strategy. Concatenating requires `.sum()`, but merging or returning the first/best requires `.max()`. Using `.sum()` for a merge strategy causes severe O(N) memory over-allocation.
**Action:** Always map the pre-allocation math directly to the behavior of the loop that populates it.

**Optimize Checkpoint Heap Allocations with Arc<Vec<T>>**
**Learning:** Changing a collection of Arc references (`Vec<Arc<T>>`) into an Arc reference of a collection (`Arc<Vec<T>>`) when read-only sharing is required drastically reduces allocator overhead. Iteration overhead remains similar (or improves due to cache locality), but thousands of individual heap allocations and atomic refcount operations during creation are entirely eliminated.
**Action:** When capturing large graph structures for snapshots, prefer contiguous vectors wrapped in a single Arc rather than arrays of individual Arc allocations.
**[Optimize TraversalIterator Vec Pre-allocation]**
**Learning:** When using `.size_hint()` to pre-allocate `Vec::with_capacity()`, initializing iterators solely for their size hint and then discarding them causes redundant graph/database lookups, introducing a severe performance regression. Additionally, refactoring closures to take mutable references rather than mutably capturing from the environment means the closures themselves no longer need to be bound with `let mut`, which prevents `clippy::unused_mut` warnings.
**Action:** Always instantiate iterators once, bind them to variables, calculate capacity using their size hints, and then consume those exact bindings. Carefully review closure bindings for unnecessary `mut` keywords when their captures change.

**[Optimize Filtering with `Vec::retain`]**
**Learning:** When retrieving a `Vec` from a lower-level API and immediately filtering it, chaining `.into_iter().filter(...).collect()` forces the allocation of a completely new `Vec` on the heap, which is an unnecessary allocation.
**Action:** Use `.retain(...)` on the existing `Vec` to filter the elements in-place. This preserves the original allocation and prevents unnecessary memory allocations in hot paths like querying the graph structure.

**Optimize query target_shards Pre-allocation with Cow**
**Learning:** In hot paths like distributed query execution (`execute` in `src/storage/sharding/executor.rs`), cloning `Vec` inputs (`shards.clone()`) creates unnecessary heap allocations and memory copies.
**Action:** Use `std::borrow::Cow` to wrap inputs that may either be borrowed directly or constructed on the fly. `Cow<'_, [T]>` enables passing slice references without cloning when available (`Cow::Borrowed(slice)`), while retaining the ability to fall back to an owned collection (`Cow::Owned(vec)`) seamlessly. This removes a heap allocation for every query execution specifying `target_shards`.

**Optimize Vec allocations with Vec::with_capacity in loops over collections**
**Learning:** Initializing a vector with `Vec::new()` and then populating it in a loop whose length is known (e.g. iterating over a `DashMap`) causes unnecessary intermediate heap reallocations.
**Action:** Use `Vec::with_capacity(collection.len())` when the target collection size is known in advance to avoid these reallocations.

**Optimize Checkpoint Serialization Vec Pre-allocation**
**Learning:** Found multiple vectors (`nodes`, `edges`, `node_versions`, `edge_versions`, etc.) in `extract_graph_data_from_snapshot` and `extract_temporal_data_from_snapshot` within `src/storage/checkpoint.rs` being created without capacity, resulting in potentially multiple heap reallocations when persisting thousands or millions of entities. `clippy::unused_doc_comments` lint caught the invalid `///` doc comment usage inside function bodies.
**Action:** Use `Vec::with_capacity(n)` instead of `Vec::new()` when initializing vectors and calculating their capacity using snapshot's `node_count`, `edge_count`, `node_version_count`, and `edge_version_count` methods. Use standard `//` for inline comments to avoid unused doc comment warnings.
