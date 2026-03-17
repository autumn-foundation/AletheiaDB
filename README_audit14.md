Wait, `FromStr` is not implemented for `NodeId`! So if a user gets an ID from a URL parameter like "42", they have to write:
`NodeId::new(param.parse::<u64>().unwrap()).unwrap()`.
That is painful DX. We should implement `FromStr` for `NodeId`, `EdgeId`, `VersionId`, `TxId` to return a `Result<Self, Error>`! Or at least `Result<Self, StorageError>`.
And `TryFrom<u64>` is also needed!

Let's check if we can add an `execute_aql` to `AletheiaDB`.
