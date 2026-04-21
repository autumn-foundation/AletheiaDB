💡 What:
Pre-allocated the `events` vector in `generate_node_narrative` using `Vec::with_capacity(history.versions.len())`.

🎯 Why:
The number of narrative events exactly matches the number of history versions returned by `get_node_history`. By pre-allocating the capacity, we avoid unnecessary intermediate heap allocations and memory reallocations inside the loop over `history.versions`. This follows the zero-cost abstraction philosophy of avoiding heap allocations in hot paths where possible.

📊 Impact:
Eliminates `O(log N)` heap allocations and memory reallocations where `N` is the number of node history versions being analyzed in the temporal narrative generation.

🔬 Measurement:
Run scoped test using `cargo test --lib --features semantic-temporal experimental::temporal::temporal_narrative`.
