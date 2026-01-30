## 2026-01-30 - [Testing Internal Logic via Child Modules]
**Learning:** Rust's module system allows child modules (like `mod tests`) to access private items of their parent. This was critical for testing `HybridTimestamp`'s overflow logic, which requires constructing invalid states via the `pub(crate)` method `new_unchecked`.
**Action:** When testing complex state machines that enforce invariants, look for `pub(crate)` constructors or private helper methods that can be accessed from a child `mod tests` to simulate edge cases (like overflows) that are impossible to reach via the public API.
