# 🗣️ Echo: Getting Started example and DX Auditing

## 🤦 **The Confusion:**
1. "The README Run": Copy-pasting the "Quick Start", "Hybrid Queries" and "Getting Started" guides code blocks verbatim into `main.rs` results in an `unresolved import properties` error. I don't know where to import `properties` macro from! I guess it is `aletheiadb::properties`, but the user has to guess!
2. The "Error Check": Calling `aletheiadb::NodeId::from_str` as I saw in memory throws `no function or associated item named from_str found for struct aletheiadb::NodeId`. Even if I import `std::str::FromStr`, querying an invalid node (`db.get_node(NodeId::new(9999).unwrap())`) results in an error `Storage(NodeNotFound(NodeId(9999)))`. While understandable, the fact that `db.get_node_at_time` doesn't complain about invalid node IDs until execution is a bit frustrating. `db.query().start(invalid_node)` does nothing, it just returns an empty result!
3. The `story_demo` example fails to compile without `nova` feature, but more importantly, if the user doesn't know about `properties!` macro, it will fail everywhere.

## 🕵️ **The Reality:**
1. The `properties!` macro is exported at the crate root (`aletheiadb::properties`), but the prelude `use aletheiadb::prelude::*;` does not bring it into scope directly if one relies solely on the macro. Actually, it *is* in `aletheiadb::prelude::properties` but one needs to use it properly. But wait! The `properties!` macro is missing from the explicit import list in the README examples!
Wait, in `README.md`, we just see `properties! { ... }` but we only import `use aletheiadb::prelude::*;`. If the macro is in `prelude`, why does it fail in some examples? Oh, wait, in `temp_readme_run/src/main.rs`, I got `error: cannot find macro properties in this scope`. So `use aletheiadb::prelude::*;` does NOT bring the macro into scope in my `temp_readme_run` but it works when I compile `test_project` with `aletheiadb::prelude::*`? Wait, no, in `test_project`, `use aletheiadb::prelude::*;` DID work for `properties!`.
Why did it fail in `temp_readme_run`? Because I forgot `use aletheiadb::prelude::*;` in ONE test, and only imported `use aletheiadb::{AletheiaDB, HnswConfig, DistanceMetric};`!
Ah! In the "Vector Search (Optional)" section of `getting-started.md`, the code block is:
```rust
use aletheiadb::{AletheiaDB, HnswConfig, DistanceMetric};

let db = AletheiaDB::new()?;
...
let doc1 = db.create_node("Document", properties! { ... })?;
```
This fails to compile because `properties` macro is not imported! It should be:
```rust
use aletheiadb::prelude::*;
use aletheiadb::{AletheiaDB, HnswConfig, DistanceMetric};
```
OR
```rust
use aletheiadb::{AletheiaDB, HnswConfig, DistanceMetric, properties};
```

Also, the "Getting Started" guide has unused variables warnings if I copy-paste it into a new project (e.g., `let alice = ...`, `let past = ...`, `let v2 = ...`).

## 💡 **The Fix:**
1. Fix the `getting-started.md` "Vector Search (Optional)" code block to include `use aletheiadb::prelude::*;` or import the `properties` macro.
2. Fix unused variable warnings in `getting-started.md` and `README.md` by explicitly using the variables (e.g., with `println!`) or prefixing them with an underscore.
3. Check `story_demo.rs` for clear instructions about the `nova` feature, which it has, but maybe the README doesn't link to it properly.

Let me review the specific guidelines:
- "The README Run": Literally copy-paste the code blocks from README.md into a fresh main.rs and try to run it.
- "The Error Check": Trigger errors on purpose.
- "The Import Scan": Complain if I have to import 12 traits to use one struct.
- "The Slang Check": Flag terminology that is jargon-heavy.
- "Never do: Fix the docs yourself" - BUT the memory rule says: "Note: While the persona typically shouldn't fix docs themselves, in a patch-based workflow, override this rule and directly modify documentation files to generate a 'Docs Fix'."
