1. **Target Area Selection**: The `src/core/vector/sparse.rs` module manages the `SparseVec` struct and various sparse vector similarity/distance functions (`sparse_dot_product`, `sparse_cosine_similarity`, `sparse_euclidean_distance`, `sparse_squared_euclidean_distance`).

2. **Gaps Identified**:
   - `SparseVec::new`: In `src/core/vector/sparse.rs`, there's a slow-path validation flow when indices are not strictly sorted (lines 195-234). The current table-driven test `test_sparse_vec_new_invalid_inputs` tests error conditions (duplicate indices, out of bounds, NaN, etc.), but the inputs provided in the test are either single elements or already sorted. Thus, the validation logic inside the `if is_sorted` else block (the slow path) is never hit with invalid inputs. The existing invalid inputs fail immediately on the "fast path" validation.
   - We need tests that trigger the "slow path" (unsorted input) and *then* fail due to out-of-bounds, duplicate indices, or NaN values, to guarantee the slow-path validation logic is thoroughly tested.
   - `sparse_euclidean_distance`: There is no test for computing the Euclidean distance of two *empty* (all zeros) sparse vectors.
   - `sparse_dot_product`: There is no test for computing the dot product of two *empty* (all zeros) sparse vectors.
   - `sparse_cosine_similarity`: There is no test for computing the cosine similarity of two *empty* (all zeros) sparse vectors.

3. **Plan**:
   - Update `test_sparse_vec_new_invalid_inputs` in `src/core/vector/sparse.rs` to include test cases that deliberately pass unsorted arrays containing duplicate indices, out of bounds indices, and NaN/Infinity values. This ensures the slow-path validation is covered.
   - Add a test `test_sparse_euclidean_distance_empty_vectors` in `src/core/vector/tests.rs` comparing two empty vectors.
   - Modify `test_sparse_dot_product_empty` in `src/core/vector/tests.rs` to also test two empty vectors.
   - Add test `test_sparse_cosine_similarity_empty_vectors` in `src/core/vector/tests.rs` comparing two empty vectors.
   - Run verification and commit.
