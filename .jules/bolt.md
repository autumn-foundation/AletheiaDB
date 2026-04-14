**Node ID iterator memory optimization**
**Learning:** Returning `Vec<NodeId>` when searching for all node IDs in a graph can result in massive allocations. When iterators exist specifically to only yield elements matching a predicate (like labels), allocating only the matched subset drastically reduces memory footprint. However, `get_nodes_by_label` clones the full node object, which is wasteful if we just want the ID. Adding `get_node_ids_by_label` ensures we filter directly on node references and only extract and collect the node IDs.
**Action:** When filtering iterating data, avoid cloning the data itself if only the ID is needed. Expose methods that directly return IDs instead of full objects, minimizing unnecessary allocations.
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

**Optimize Traversal Plan Serialization Vec Pre-allocation**
**Learning:** Initializing the serialized traversal plan with `Vec::new()` and repeatedly pushing/extending primitive byte arrays (`u32`, `u16`, `[u8]`) into it inside nested loops creates an arbitrary number of heap reallocations because the byte capacity size isn't known ahead of time.
**Action:** When serializing structs into byte arrays, always calculate the exact required capacity first using iterator combinators (e.g., `.sum::<usize>()`) over the inputs (e.g., node paths, string lengths) and initialize the buffer with `Vec::with_capacity(capacity)`.
**[DashMap Guard iterators and .cloned()]**
**Learning:** In AletheiaDB, `CurrentIndexes` iterators (`iter_nodes`, `iter_edges`) yield `impl Deref<Target = Node>` representing DashMap guards, not simple references (`&T`). Thus, attempting to apply `.cloned()` fails to compile since it strictly requires `&T`.
**Action:** Always use `.map(|n| n.clone())` instead of `.cloned()` when iterating over DashMap guards or opaque `impl Deref` types.

**[Optimize Vec allocations with Vec::with_capacity and exact size hints]**
**Learning:** In Rust, replacing an idiomatic `.collect::<Vec<_>>()` chain with a manual `for` loop and `Vec::with_capacity()` is often a de-optimization. Iterators implementing `ExactSizeIterator` or `TrustedLen` (e.g., from slices or `std::vec::IntoIter`) automatically pre-allocate perfect capacity and elide bounds checks during `.collect()`. However, when `.filter(...)` is introduced into an iterator chain, the exact size hint is lost, causing `.collect()` to dynamically reallocate.
**Action:** When filtering a collection of known maximum size into a `Vec`, manually pre-allocate `Vec::with_capacity(collection.len())` and use a `for` loop (or `.extend()`) to avoid all intermediate heap reallocations.

**Optimize Tokenization in Temporal Parser**
**Learning:** `tokenize_temporal_keywords` in `src/sql/temporal_parser.rs` previously copied substrings by manually advancing through a `char_indices().collect::<Vec<_>>()` and creating a `String` inside loops (`let word: String = chars[start..i].iter().map(|(_, c)| c).collect()`). This resulted in a heap allocation for every word parsed.
**Action:** Used `sql.char_indices().peekable()` instead of collecting it. Leveraged `.len_utf8()` to calculate byte boundaries on the fly without allocations. Used a string slice `&sql[start..end]` to avoid `String` allocation for every word parsed, making parsing `0-cost` for non-matching tokens.

**[Zero-Cost Lexer Lookahead]**
**Learning:** `Peekable::clone()` is not a zero-cost abstraction when parsing strings, especially since it clones the underlying iterator state on every single token. For simple ASCII characters (like `-`, `/`, `*`, `0`..`9`), `input.as_bytes().get(idx + 1)` is much faster and completely avoids heap allocations and iterator cloning overhead.
**Action:** When doing lookaheads in a lexer over a `String` or `&str`, prefer slicing the underlying byte array `as_bytes()` if the characters you're looking for are strictly single-byte ASCII.
**[QueryRow Entity Extraction]**
**Learning:** Destructuring a struct (like `QueryRow`) and pattern matching directly on its owned components (`EntityResult`) inside a loop completely eliminates the need for expensive heap allocations caused by intermediate `.clone()` calls on nested data structures like HashMaps (properties). Rust's move semantics are the ultimate zero-cost abstraction for consuming data iterators.
**Action:** When mapping over iterators or structs that are fully consumed, never use `as_ref().map(|x| x.clone())`. Instead, destructure the container and take ownership by value.
**[Optimizing BFS Traversal]**
**Learning:** Using `iterator.chain()` to iterate over neighbors in BFS traversal instead of allocating intermediate arrays reduces heap allocations without compromising performance or logic.
**Action:** When finding multiple `.collect::<Vec<_>>()` and `.extend()` combinations, use `.chain()` whenever possible to eliminate allocations.
