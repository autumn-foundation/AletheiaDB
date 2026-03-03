Ah, so `AletheiaDB` has a method `read`, which takes a closure that receives a `ReadTransaction`. `ReadTransaction` implements `ReadOps`, which is defined in `aletheiadb::api::ops::ReadOps`.
"Why do I need to import traits to use one struct?"
Echo's job is to flag this.
"The "Import Scan": Complain if I have to import 12 traits to use one struct."

Let's do the "Walkthrough":
Scenario: "I am a new user trying to use the API based *only* on the public docs/examples."
1. I open README.md, go to Quick Start.
2. I try `echo_test.rs` (Basic Graph Operations). It works fine.
3. I try time-travel queries. Works.
4. I try Vector search. The example has:
```rust
use aletheiadb::prelude::*;
use aletheiadb::{HnswConfig, DistanceMetric};
use aletheiadb::index::vector::temporal::TemporalVectorConfig;
```
It works, but it's annoying to import `TemporalVectorConfig` from `aletheiadb::index::vector::temporal::TemporalVectorConfig`.

5. I try MCP Server: I run `cargo run --bin aletheia-mcp --features mcp-server`
Output: `Error: ConnectionClosed("initialized request")`.
Wait, this isn't an error per se, it's just that it requires stdio to communicate with the client, and I ran it directly from the terminal without standard input. This is expected.

6. Let's look at the "Error Check" again.
Config reading a nonexistent file: "IoError("No such file or directory (os error 2)")".
Let's see the error type returned by `from_toml_file`.
