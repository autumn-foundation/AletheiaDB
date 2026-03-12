# 🗣️ Echo: Getting Started example is broken

## 🤦 The Confusion
I wanted to try the Experimental Features and Embedding Generation examples in the `README.md`. I copy and pasted the code under "Narrative Generation (Experimental)" into a fresh `main.rs`.

When I run `cargo run` after enabling the `nova` feature, I hit a weird compile error:

```rust
error[E0282]: type annotations needed
  --> src/main.rs:11:30
   |
11 |     let node_id = db.write(|tx| {
   |                              ^^
12 |         tx.create_node("Person", properties! {
   |         -- type must be known at this point
```

Why is it asking for a type annotation on `tx`? This is completely unreadable for a beginner!

And it gets worse. I tried to copy-paste the "Embedding Generation (Optional)" example. I immediately got unresolved import errors:

```rust
error[E0432]: unresolved import `aletheiadb::embeddings`
 --> src/main.rs:2:17
  |
2 | use aletheiadb::embeddings::{EmbeddingService, providers::openai::*};
  |                 ^^^^^^^^^^ could not find `embeddings` in `aletheiadb`
```

I was about to give up until I read *inside* the `main` function on line 7: `// Enable in Cargo.toml: features = ["embedding-openai"]`. The instruction to fix the error is buried *after* the code that causes the error!

## 🕵️ The Reality
1. For Narrative Generation: Rust's compiler can't infer the type of `tx` in `db.write(|tx| { ... })` properly when `tx.create_node` is the last statement without a semicolon because the type of `node_id` is not explicitly declared. `db.write()` is generic, and the compiler fails to infer the return type of the closure without explicit type information for `node_id`. This means the example code *literally doesn't compile* out of the box.

2. For Embedding Generation: The `use` statements at the top of the file require a feature flag that isn't mentioned until line 7, *inside* the function body. A user copy-pasting the file will see compile errors on line 2 and might never read down to line 7 to find out why.

## 💡 The Fix
Update the `README.md` to fix these DX friction points:

1. **Narrative Generation:** Provide explicit type annotations so the compiler doesn't choke.
Change:
```rust
    let node_id = db.write(|tx| {
```
To:
```rust
    let node_id: NodeId = db.write(|tx| {
```
Or: `let node_id = db.write(|tx: &mut WriteTransaction| {`

2. **Embedding Generation:** Move the feature requirement comment to the very top of the snippet, *before* the `use` statements.
Change:
```rust
// [dependencies]
// tokio = { version = "1", features = ["full"] }
// aletheiadb = { version = "0.1", features = ["embedding-openai"] }
use aletheiadb::{AletheiaDB, properties};
```
(Include the exact `Cargo.toml` requirements at the top of the block, just like the Narrative Generation example does).
