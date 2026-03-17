Yes, there is no `execute_aql`.

Another friction point:
`db.traverse_and_rank` takes a starting node id, a relationship label, an embedding vector, and a limit.
But wait, how does a user construct an `embedding` vector if they just want to pass `vec![0.1f32; 10]` to `create_node`?
We saw:
```rust
let doc_id = db.create_node("Document", properties!{
    "embedding" => &embedding[..]
});
```
Wait, the `properties!` macro converts elements using `Into<PropertyValue>`. We saw an error when `embedding` was `f64`. But for `f32`, `&[f32]` works!
So that's fine.

Let's look at `PropertyMap::new()` versus `properties!{}`.
Does the README still have `PropertyMap::new()` everywhere?
In the `test_transactions` example:
```rust
    // Explicit write transaction with multiple operations
    db.write(|tx| {
        let node1 = tx.create_node("Event", PropertyMap::new())?;
        let node2 = tx.create_node("Event", PropertyMap::new())?;
        tx.create_edge(node1, node2, "FOLLOWS", PropertyMap::new())
    })?;
```
And earlier:
```rust
let alice_id = db.create_node("Person", properties! {
    "name" => "Alice",
    "age" => 30,
})?;
```
So both are used. `properties!{}` is more concise, but `PropertyMap::new()` is okay for empty maps. Even better would be `properties!{}` without elements for empty. Wait, `properties!{}` expands to an empty map? Let's check `properties!` macro in `src/core/property.rs`.
