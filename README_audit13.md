Wait, `properties!{}` works because the macro allows zero or more elements. It will expand to `PropertyMapBuilder::new().build()`. The README examples use `properties! {}` correctly:
```rust
db.create_edge(alice_id, bob_id, "KNOWS", properties! {})?;
```

So the main friction points I've found for Echo:
1. `NodeId` from an integer is painful (`NodeId::new(42).unwrap()`). And there's no `From<u64>` or `TryFrom<u64>`. Let's add `TryFrom<u64>` and `FromStr` to make it easier to parse from IDs. Wait, if we use `From<u64>` we could just panic, but `TryFrom` is better. Or a macro `node!(42)` which asserts statically or panics. But users can just use `NodeId::try_from(42)?` or similar. Actually, adding `TryFrom<u64>` to `NodeId`, `EdgeId`, `VersionId`, `TxId` is a great DX fix. Also a simple string parser like `FromStr` would help web developers immensely (since `NodeId` is commonly extracted from URL paths like `/api/nodes/42`).

2. AQL (Query Language) section in the README has no Rust code! It just shows Cypher but not how to execute it in AletheiaDB. I should add `db.execute_aql(&str) -> Result<QueryResults>` to make it dead simple for users:
```rust
let results = db.execute_aql("MATCH (n:Person {name: 'Alice'}) RETURN n")?;
```

3. Export `parse_query` in `aletheiadb::prelude`? If I add `db.execute_aql` they don't even need `parse_query` for basic string execution! `execute_aql` is the ultimate DX fix.

Let's test if `NodeId` implements `FromStr`.
