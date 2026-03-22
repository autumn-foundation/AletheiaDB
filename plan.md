1. **Insert explicit matching tests**:
   - I will use `sed -i` to append test cases directly to the `sentry_tests` module of `src/core/graph.rs` ensuring they are robust enough to kill the remaining mutants listed in the prompt (`matches_label`, `with_metadata`, `get_property`, `has_label`, `has_label_str`, `connects`, `fmt::Debug`).

2. **Execute tests**:
   - Run `cargo test --lib core::graph` to make sure the modified tests pass.

3. **Run mutants evaluation**:
   - Execute `cargo mutants --list --file src/core/graph.rs` to review. Note we use `--list` or very localized runs because the workspace can time out `cargo mutants`.

4. **Verify Lints & formatting**:
   - Run `cargo clippy --all-targets --all-features -- -D warnings` to verify code quality.
   - Run `cargo fmt --all` to format correctly.

5. **Append findings to Journal**:
   - Use `run_in_bash_session` to append a summary of findings using the required Sentinel format into `.jules/sentinel.md`.

6. **Complete pre-commit steps**:
   - Complete pre commit steps to make sure proper testing, verifications, reviews and reflections are done.

7. **Submit PR**:
   - Execute the `submit` tool to file a PR with title `🤖 Sentinel: [brief description]` containing the required sections.
