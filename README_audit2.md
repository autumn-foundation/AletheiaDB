So `test_transactions2` compiles fine! Wait, why did `test_error3` fail?
Oh! `test_error3.rs` had:
```rust
use aletheiadb::AletheiaDB;
use aletheiadb::properties;
```
which omitted `use aletheiadb::prelude::*;`. Okay, so `test_transactions` worked previously, `test_transactions2` works, my mistake.

Let's do the other Echo checks.
- 🤦 **The Confusion:**
"Why do I need to initialize Graph before Config?"
"The example uses v0.1 but Cargo.toml is v0.2."
"This error message just says doh!."

Let's look at `NodeId::new(100)`:
```rust
    let db = AletheiaDB::new().unwrap();
    match db.get_node(NodeId::new(100)) {
        Ok(_) => println!("Found node"),
        Err(e) => println!("Error: {}", e),
    }
```
Wait, `NodeId::new(100)` does not return a `NodeId`, it returns a `Result<NodeId>`. Let's check `NodeId::new`.
