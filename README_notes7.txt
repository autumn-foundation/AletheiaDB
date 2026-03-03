Let's see the error for getting a node that doesn't exist:
`let res = db.get_node(node_id); println!("{:?}", res);`
Output: `Err(Storage(NodeNotFound(NodeId(0))))`
This message "Err(Storage(NodeNotFound(NodeId(0))))" is quite clear!

Wait, what if they try to use the query builder?
"The "Import Scan": Complain if I have to import 12 traits to use one struct."
Let's look at the imports in the `Hybrid Queries` example.
```rust
// No imports listed in this snippet!
```
Wait! The `Vector Search with HNSW` example:
```rust
use aletheiadb::prelude::*;
use aletheiadb::{HnswConfig, DistanceMetric};
use aletheiadb::index::vector::temporal::TemporalVectorConfig;
```
It requires 3 lines of imports just to enable vector search, and they are deep inside `aletheiadb::index::vector::temporal::TemporalVectorConfig`.
Could we re-export them to the prelude or crate root?
"Complain if I have to import 12 traits to use one struct."
"The "Slang Check": Flag terminology that is jargon-heavy (e.g., "bitemporal adjacency" vs "history")."
- "valid time", "transaction time", "Anchor+Delta Compression", "Striped lock-free ring buffer", "Bi-Temporal Model".
- "Semantic Drift Tracking", "DriftMetric", "TransactionInterval", "Hybrid Logical Clock"
Are these too jargon-heavy?
The README has:
"Anchor+Delta Compression"
"Bi-Temporal Model"
"Semantic Drift Tracking"

What about the "Error Check"? Trigger errors on purpose. Are the messages helpful?
Let's try to trigger an error when parsing a bad config file.
