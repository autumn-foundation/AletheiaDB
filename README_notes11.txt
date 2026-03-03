Okay, what are the friction points?
1. **The Walkthrough (Embedding Generation)**:
   - "Tried to run the `Embedding Generation (Optional)` example. Compiler said `unresolved module tokio`."
   - "Turns out I needed to add the `tokio` crate to `Cargo.toml`. The README only has a comment saying `// Note: Requires tokio dependency in Cargo.toml`, but people copy-paste without reading."
   - And the return type `Result<(), Box<dyn std::error::Error>>` will cause a type alias generic argument mismatch error (`E0107`) if the user imports the `aletheiadb::prelude::*`, which they probably already did in previous examples. It needs to be `std::result::Result<(), Box<dyn std::error::Error>>` like in other examples.

2. **The Friction Points (Error Check)**:
   - "Config file error just says `IoError("No such file or directory (os error 2)")`."
   - "This is unhelpful. It should say something like `File not found: nonexistent.toml` instead of spitting out `os error 2`."

3. **The Import Scan (Vector Search)**:
   - "I have to import `HnswConfig`, `DistanceMetric`, and `TemporalVectorConfig` from three different deep paths (`aletheiadb::index::vector::temporal::TemporalVectorConfig`) just to enable vector indexing."
   - "These should be exported in the prelude or crate root, otherwise it's just `use`-statement spaghetti."

4. **The Slang Check**:
   - The docs use `valid time` and `transaction time`. While accurate bitemporal terminology, users just think of it as "when it happened" and "when it was recorded".
   - The term "Anchor+Delta Compression" might be a bit jargon-heavy instead of "incremental backups/snapshots".
   - "Semantic Drift Tracking".

Wait, are there other friction points?
Let's check `AletheiaDB::new().unwrap()` in "Time-Travel Queries" vs `AletheiaDB::with_unified_config(config)?` in "Configuration".
In "Time-Travel Queries":
```rust
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Setup: Create a node
    let db = AletheiaDB::new().unwrap();
```
It uses `.unwrap()` inside a function that returns `Result`. Why not `?`?

Wait, what does the memory say?
"The 'embedding-onnx' feature, along with its 'tokenizers' and 'ort' dependencies, were removed from AletheiaDB because they were non-functional placeholders that introduced an unmaintained crate vulnerability (paste crate, RUSTSEC-2024-0436)."
But the README still lists them!
```toml
| `embedding-onnx` | ONNX local inference (⚠️ placeholder) | `embeddings`, `ort`, `tokenizers` |
```
And Cargo.toml:
```toml
ort = { version = "2.0.0-rc.0", optional = true }
tokenizers = { version = "0.22", optional = true }
```
Wait, if they were removed, the README should not list them! Ah, but does Cargo.toml still have them?
Yes, `ort` and `tokenizers` are still in `Cargo.toml` in my `grep "version =" Cargo.toml` output above!
Wait, but if they were removed, I shouldn't be seeing them? Or maybe the memory says "were removed", meaning I need to make sure they are NOT there, or if they are, I should remove them? No, "The 'embedding-onnx' feature, along with its 'tokenizers' and 'ort' dependencies, were removed from AletheiaDB". This means I need to remove them? Or they were removed, but the README still lists them so I need to update the README? Let me check `Cargo.toml` manually.
