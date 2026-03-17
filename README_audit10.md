Wait, shouldn't `db` have a convenience method to parse and execute a query string?
`db.execute_aql("MATCH (n:Person) RETURN n")` or `db.run("MATCH (n:Person) RETURN n")`?
If not, then executing a Cypher query takes:
```rust
use aletheiadb::query::parse_query;

let query = parse_query("MATCH (n:Person) RETURN n")?;
let results = db.execute_query(query)?;
```
This is a small friction point, but maybe not a major one. But wait, what if I can just add `db.execute_aql(&str) -> Result<QueryResults>`?

What about the DX Audit Report structure?
I need to:
Create an Issue (or PR with a 'Docs Fix' request):
- Title: "🗣️ Echo: Getting Started example is broken" (or something describing the friction)
- Description:
* 🤦 **The Confusion:**
* 🕵️ **The Reality:**
* 💡 **The Fix:**

Let's look for other friction points. What about constructing `NodeId`?
"I am a new user trying to get a node by ID. The documentation shows `db.get_node(alice_id)`, but when I try to hardcode an ID for testing, I have to write `NodeId::new(42).unwrap()`. It's verbose and feels un-idiomatic. Why can't I just use `42.into()`?"

Let's see if we can fix this by adding `From<u64>` for `NodeId`. Wait, we can't because of `MAX_VALID_ID` validation!
But we CAN add `TryFrom<u64> for NodeId`. Let's check if it exists. We already grepped for `TryFrom` in `src/core/id.rs` and it returned nothing. Adding `TryFrom<u64>` would be great. But wait, AletheiaDB uses `NodeId::new(...)` returning a `Result`. So `TryFrom` would just wrap `NodeId::new`.
But what if we implement `From<u64>` and just `panic` if it's over `MAX_VALID_ID`? Standard library types sometimes do that, but `TryFrom` is safer. What about a macro like `node_id!(42)`? Or maybe I'm overthinking it.

Let's do "The Import Scan".
If I want to run a query string, I have to import `aletheiadb::query::parse_query`. Is it in the prelude?
