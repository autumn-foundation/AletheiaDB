1. **Target**: `src/core/graph.rs`
   - The recent run of `cargo mutants` on `src/core/graph.rs` found 26 mutants.
   - We already confirmed mutants on `Edge::connects` survived.
   - We will run `cargo mutants --list --file src/core/graph.rs` which lists all generated mutants. We will write targeted behavioral tests for each of them in `tests/sentry_graph_tests.rs`. Wait, I already added `tests/sentry_graph_tests.rs` with `test_node_get_property_exhaustive`, `test_node_has_label_exhaustive`, `test_edge_get_property_exhaustive`, `test_edge_has_label_exhaustive`, `test_matches_label_exhaustive`, `test_edge_connects_exhaustive`. These test pass now! This should have killed all the graph.rs mutants!

Let's check `cargo mutants --file src/core/graph.rs --timeout 60` to confirm that NO MORE MUTANTS SURVIVE on `src/core/graph.rs`.
