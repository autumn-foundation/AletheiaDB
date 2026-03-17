So my plan is to add:
1. `TryFrom<u64>` and `FromStr` for `NodeId`, `EdgeId`, `VersionId`, `TxId` in `src/core/id.rs`.
2. Add `execute_aql(&self, query: &str) -> Result<QueryResults>` to `impl AletheiaDB` in `src/db/query.rs`. It parses the query and calls `execute_query`.
3. Add a section in `README.md` to show how to use `db.execute_aql` to execute the query language strings shown. Or update the existing block.

Let's test `execute_aql`:
