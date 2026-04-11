import re
import os

files = [
    "src/query/executor/mod.rs",
    "src/storage/sharding/coordinator.rs",
]

for path in files:
    with open(path, "r") as f:
        content = f.read()

    # The codecov failure is because the new doc comments added lines that are somehow being marked as "not covered"
    # This happens in Rust/Codecov when standalone line comments are interpreted as executable lines by the coverage tool.
    # To fix this, we append the comments to the end of the relevant executable line, or use `// comment` instead of `///` if it's not a doc comment.
    # But wait, `///` are doc comments, they shouldn't be counted in coverage.
    # The memory states:
    # "In Rust projects tracked by Codecov (or similar coverage tools), adding standalone line comments (e.g., `// Optimization note`) can sometimes be flagged as uncovered lines, artificially lowering patch coverage and failing CI checks. To prevent this, append the comment to the end of the relevant executable code line (e.g., `let x = 1; // comment`)."
    # Are `///` doc comments flagged as uncovered lines?
    # Let's check lines 137-141 in `src/query/executor/mod.rs`:
    # 133:     /// Create a new query executor
    # 134:     /// Create a new query executor with default config.
    # 135:     ///
    # 136:     /// # Details
    # 137:     ///
    # 138:     /// The default configuration prioritizes minimal memory usage but does not
    # 139:     /// enable parallel execution.
    # 140:     pub fn new(current: Arc<CurrentStorage>, historical: Arc<RwLock<HistoricalStorage>>) -> Self {

    # Wait, the failure is "Added lines #L137 - L141 were not covered by tests".
    # Lines 137-141 correspond exactly to the added doc comments.
    # Is it possible that `///` doc comments themselves are counted? Yes, apparently.
    # We should clean up the duplicate `/// Create a new query executor` vs `/// Create a new query executor with default config.`.

    # Let's see if there are empty lines being added.

    content = content.replace(
        "    /// Create a new query executor\n    /// Create a new query executor with default config.\n    ///\n    /// # Details\n    ///\n    /// The default configuration prioritizes minimal memory usage but does not\n    /// enable parallel execution.",
        "    /// Create a new query executor with default config.\n    ///\n    /// # Details\n    ///\n    /// The default configuration prioritizes minimal memory usage but does not\n    /// enable parallel execution."
    )

    content = content.replace(
        "    /// Create an executor with custom configuration\n    /// Create a new query executor with custom config.\n    ///\n    /// # Details\n    ///\n    /// Use this to modify default batch sizes or concurrency targets before\n    /// executing queries.",
        "    /// Create a new query executor with custom config.\n    ///\n    /// # Details\n    ///\n    /// Use this to modify default batch sizes or concurrency targets before\n    /// executing queries."
    )

    with open(path, "w") as f:
        f.write(content)
