1. **Optimize `VectorRerankIterator` in `src/query/executor/iterators.rs`**
   - The current code constructs an intermediate `Vec<(QueryRow, f32)>` just to convert it to an iterator for `self.sorted`:
     ```rust
     let sorted_rows: Vec<(QueryRow, f32)> = heap
         .into_sorted_vec()
         .into_iter()
         .map(|Reverse(item)| (item.row, item.score))
         .collect();
     self.sorted = Some(sorted_rows.into_iter());
     ```
   - Change `VectorRerankIterator::sorted` from `Option<std::vec::IntoIter<(QueryRow, f32)>>` to `Option<std::vec::IntoIter<std::cmp::Reverse<ScoredRow>>>`.
   - Update `next` implementation to handle `std::cmp::Reverse<ScoredRow>` instead of `(QueryRow, f32)`. This completely eliminates the extra vector allocation of size `k`.
   - Update `self.sorted = Some(heap.into_sorted_vec().into_iter())` instead of `.collect()`.

2. **Complete pre-commit steps to ensure proper testing, verification, review, and reflection are done.**
   - Run `cargo clippy --all-targets --all-features -- -D warnings`.
   - Run `cargo test`.
   - Run `cargo fmt --all`.

3. **Create a PR with the title '⚡ Bolt: Optimize VectorRerankIterator by removing intermediate Vector allocation'**
   - The PR description will contain:
     * 💡 What: Changed `VectorRerankIterator` to iterate directly over `BinaryHeap::into_sorted_vec` without an intermediate mapping to a new `Vec`.
     * 🎯 Why: To avoid allocating an extra `Vec` of up to size `k` every time a vector rerank query executes.
     * 📊 Impact: Removes 1 heap allocation per vector rerank execution.
     * 🔬 Measurement: Run bench `process_data` (or test rerank queries) to verify correctness and see fewer allocs.
