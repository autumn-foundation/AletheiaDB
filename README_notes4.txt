Okay, the example from the README has a couple of issues!
Let's recall the embedding example exactly as in the README.

```rust
use aletheiadb::{AletheiaDB, properties};
use aletheiadb::embeddings::{EmbeddingService, providers::openai::*};
use std::sync::Arc;

// Note: Requires `tokio` dependency in Cargo.toml
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
```
The problem is that if someone uses `tokio` they have to add `tokio` to their `Cargo.toml`. The README says: "Note: Requires `tokio` dependency in Cargo.toml". But also, `Result<(), Box<dyn std::error::Error>>` will fail if they have `aletheiadb::prelude::*` imported because `Result` will be shadowed, but here they only import `AletheiaDB` and `properties` so `Result` resolves to `std::result::Result` in standard prelude, so that is actually correct for the user *unless* they import `prelude::*`! Wait! If they only use `aletheiadb::{AletheiaDB, properties}`, then `Result` is `std::result::Result` and it compiles fine! Let's check:
I tested `echo_test12` with `aletheiadb::{AletheiaDB, properties}` and `tokio` was unresolved because I didn't add tokio to `src/bin` Cargo.toml, but for a user who adds tokio, it compiles. The error "OPENAI_API_KEY environment variable not set" is expected runtime behavior when no key is provided.

Wait, are there other friction points in the README?
Let's check the MCP server:
`cargo run --bin aletheia-mcp --features mcp-server`
Let's check if the binary exists.
