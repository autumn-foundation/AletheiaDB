Error message is: `ValidationFailed { reason: "Edge source node NodeId(1) does not exist" }`. That's actually very clear and helpful.

What about `vector_index`?
If you forget to enable a vector index, the hint says:
`Call db.vector_index("embedding").hnsw(...).enable() first`
This was fixed previously by Echo.

Let's check the API for `NodeId::new(id)`
Wait, the README uses:
```rust
    let alice_id = db.create_node("Person", properties! {
        "name" => "Alice",
        "age" => 30,
    })?;
```
And then passes `alice_id` around. This is fine. BUT when a user needs to construct a `NodeId` from an integer (e.g. from a web request, or hardcoding in a test), they have to do: `NodeId::new(42).unwrap()`.
Wait, why doesn't `NodeId` have `From<u64>` or `TryFrom<u64>`?

Let's check `AQL` (AletheiaDB Query Language).
Is there an easy way to run queries? The README shows:
```cypher
-- Basic graph query
MATCH (n:Person {name: "Alice"})-[:KNOWS]->(friend:Person)
RETURN friend
```
Wait, how do I actually execute this string in Rust?
There is no example of executing AQL via a string in the README! The README says:
"AletheiaDB supports a Cypher-like query language with temporal and vector extensions:"
And shows SQL/Cypher-like code, but no Rust API call like `db.execute_aql("...")`.

Let's find if there is an AQL execute method.
