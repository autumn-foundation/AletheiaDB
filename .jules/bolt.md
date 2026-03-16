**Avoid intermediate Vec allocations when parsing queries**
**Learning:** `cleaned.split_whitespace().collect::<Vec<_>>().join(" ");` creates an unnecessary `Vec` allocation on the heap, which isn't necessary for simple string transformations.
**Action:** Use a pre-allocated string with `String::with_capacity(cleaned.len())` and a loop over the word iterator to concatenate spaces and words directly.

**Pre-allocated vectors in sharding scatter logic**
**Learning:** Found multiple vectors (`results`, `failures`, `aggregated`) in the critical path (`execute` and `aggregate_results`) being created without capacity, resulting in potentially multiple heap reallocations. `results` and `failures` size depends directly on `target_shards.len()`. The `aggregated` vector size depends on the sizes of all result data fragments.
**Action:** Always pre-calculate the required vector capacity when creating `Vec`s in a collection loop, using methods like `.sum()` on `.map(|x| x.len())` for collection concatenation.

**Pre-allocated vector sizes based on strategy**
**Learning:** When pre-allocating vectors based on multiple inputs, be mindful of the aggregation strategy. Concatenating requires `.sum()`, but merging or returning the first/best requires `.max()`. Using `.sum()` for a merge strategy causes severe O(N) memory over-allocation.
**Action:** Always map the pre-allocation math directly to the behavior of the loop that populates it.
