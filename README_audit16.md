Okay, I am ready to write the DX Audit report and fix these friction points.
Echo's Report should mention:
- Friction 1: Constructing `NodeId` from integers or strings is overly verbose due to `NodeId::new(val)` returning a `Result`, and the lack of standard conversion traits like `TryFrom<u64>` or `FromStr`.
- Friction 2: The README has an entire section on "Query Language (AQL)" showing SQL/Cypher-like code, but ZERO Rust code explaining how to actually run those queries. Users have to guess `db.execute_aql()` (which doesn't exist) or dig through source code to find `parse_query` and `db.execute_query()`.
