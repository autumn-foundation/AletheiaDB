Findings from the README run:
1. `echo_test6.rs` (Narrative Generation example):
Fails to compile/run correctly without the `nova` feature, and when it fails at runtime, it panics with "NarrativeGenerator requires the 'nova' feature". But this is expected per instructions: "Turns out I needed to enable feature nova... Add a huge banner in README saying 'REQUIRES FEATURE NOVA'." Wait! The README already has the banner:
`// ⚠️ REQUIRES FEATURE: nova`
Is there a problem? Wait, the README does say "REQUIRES FEATURE NOVA" but the compilation throws a deprecation warning and then the code panics. Let's look closely at the instructions:

"Create an Issue (or PR with a 'Docs Fix' request):
Title: "🗣️ Echo: Getting Started example is broken"
Description:
* 🤦 **The Confusion:** "Tried to run the `story_demo`. Compiler said `NarrativeGenerator` not found."
* 🕵️ **The Reality:** "Turns out I needed to enable feature `nova`."
* 💡 **The Fix:** "Add a huge banner in README saying 'REQUIRES FEATURE NOVA'.""

Wait! That was just an *example* of what Echo's Daily Process looks like. It is an *example* scenario ("Scenario: 'I am a new user trying to add `Nova`'s story feature.'")
I shouldn't literally apply the "story_demo" fix if it's already there! The instructions say: "Your mission is to audit the "Developer Experience" (DX). You verify that examples work, error messages are helpful, and APIs are intuitive."

Let's look at the friction points I found by literally copy-pasting code blocks from `README.md` into a fresh `main.rs` and trying to run it.

1. **Embedding Generation (Optional)** example:
```rust
use aletheiadb::{AletheiaDB, properties};
use aletheiadb::embeddings::{EmbeddingService, providers::openai::*};
use std::sync::Arc;

// Note: Requires `tokio` dependency in Cargo.toml
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
```
The user runs this. But wait, if they follow the `Quick Start` they'll copy it. If they have `prelude::*` imported (which they might from earlier examples, or maybe they just copy-paste exactly this block), does it compile?
Yes, if they copy-paste exactly this block AND add `tokio` dependency, it compiles, except wait: `AletheiaDB` in `AletheiaDB::new()?` requires `use aletheiadb::AletheiaDB` which is there. But they also need `aletheiadb::embeddings` which requires the `embedding-openai` feature. The comment says `// Enable in Cargo.toml: features = ["embedding-openai"]`.
However, if they don't have the `tokio` crate installed, it fails with "error[E0433]: failed to resolve: use of unresolved module or unlinked crate `tokio`". They have to figure out to add it.
Also, `Result<(), Box<dyn std::error::Error>>` fails if they have `use aletheiadb::prelude::*;` because of `Result` alias, but that's only if they combined imports.

Wait! Let's check the other examples in the README.
2. **Observability Example**:
```rust
use aletheiadb::observability;

fn main() {
    // Initialize observability (call once at startup)
    let config = observability::Config::from_env();
    observability::init(config);

    let db = aletheiadb::AletheiaDB::new().unwrap();

    // Metrics automatically collected
    // Check for critical errors
    let metrics = observability::metrics();
...
```
If you copy-paste this without the `observability` feature enabled, you get:
`error[E0432]: unresolved import aletheiadb::observability`
The README says `// Enable in Cargo.toml:` but the user copy-pasting the code directly gets a compiler error. There is no `⚠️ REQUIRES FEATURE: observability` in the snippet itself! Wait, above it says:
```bash
# Enable in Cargo.toml:
features = [
    "observability",              # Core: structured logging + metrics
```
but in the actual Rust code block, there is no comment about it.

3. **Transactions Example**:
```rust
use aletheiadb::prelude::*;

// Explicit read transaction
let result = db.read(|tx| {
    tx.get_node(alice_id).map(|node| node.label.clone())
})?;

// Explicit write transaction with multiple operations
db.write(|tx| {
    let node1 = tx.create_node("Event", PropertyMap::new())?;
    let node2 = tx.create_node("Event", PropertyMap::new())?;
    tx.create_edge(node1, node2, "FOLLOWS", PropertyMap::new())
})?;
```
Notice: `PropertyMap::new()` is used, but it's not imported in the example or available in prelude?
Let's check `aletheiadb::prelude::*`.
