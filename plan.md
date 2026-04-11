1.  **Refactor Ceremonial Assertions in `src/api/transaction/write/tests.rs`**
    -   Use `replace_with_git_merge_diff` to replace weak `assert!(...is_ok())` calls with `...unwrap()` and explicit assertions where applicable.
2.  **Refactor Ceremonial Assertions in `src/db/tests.rs`**
    -   Use `replace_with_git_merge_diff` to replace weak `assert!(...is_ok())` calls with `...unwrap()`.
3.  **Refactor Ceremonial Assertions in `src/embeddings/providers/*.rs`**
    -   Use `replace_with_git_merge_diff` to replace weak `assert!(...is_ok())` calls with `...unwrap()`.
4.  **Refactor Ceremonial Assertions in `src/experimental/fishing.rs`**
    -   Use `replace_with_git_merge_diff` to replace weak `assert!(...is_ok())` calls with `...unwrap()`.
5.  **Refactor Ceremonial Assertions in `src/experimental/sentinel.rs`**
    -   Use `replace_with_git_merge_diff` to replace weak `assert!(...is_ok())` calls with `...unwrap()`.
6.  **Complete pre-commit steps to ensure proper testing, verification, review, and reflection are done.**
7.  **Submit Pull Request**
    -   Use `run_in_bash_session` to run `bash scripts/worktree-new.sh jules-elenchus-ceremonial-assertions`.
    -   Commit changes.
    -   Use `run_in_bash_session` to run `bash scripts/worktree-pr.sh` with title `"⚔️ Elenchus: Ceremonial Assertions Audit"`.
