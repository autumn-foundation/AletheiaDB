**Single-Pass Row Collection for Results**
**Learning:** `Iterator::collect::<Vec<_>>()` forces an O(N) heap allocation when processing data. For query execution result aggregation into columnar structures, this creates unnecessary memory overhead.
**Action:** Replace `collect()` followed by iteration with a single `while let` loop over the iterator, pushing fields directly into lazy-initialized columnar `Vec`s (`Option<Vec<T>>`). This eliminates the intermediate row vector.
