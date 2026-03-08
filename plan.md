1. Use `run_in_bash_session` to execute `mkdir -p src/index/temporal`.
2. Use `run_in_bash_session` to execute `sed -n '1,1334p' src/index/temporal.rs > src/index/temporal/mod.rs` to extract the core code.
3. Use `run_in_bash_session` to execute `sed -n '1336,4207p' src/index/temporal.rs > src/index/temporal/tests.rs` to extract the tests code.
4. Use `run_in_bash_session` to execute `echo 'mod tests;' >> src/index/temporal/mod.rs`.
5. Use `run_in_bash_session` to execute `rm src/index/temporal.rs`.
6. Use `read_file` to verify the contents of the newly written `src/index/temporal/mod.rs` and `src/index/temporal/tests.rs`.
7. Use `run_in_bash_session` to execute `cargo check`.
8. Use `run_in_bash_session` to execute `cargo test --lib index`.
9. Use `run_in_bash_session` to execute `cargo test`.
10. Use `run_in_bash_session` to execute `cargo doc --all-features --no-deps`.
11. Complete pre-commit steps to ensure proper testing, verification, review, and reflection are done.
