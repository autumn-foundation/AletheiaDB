So `NodeId::new` returns a `Result<Self, StorageError>`. It's a bit annoying for simple hardcoded IDs: `NodeId::new(100).unwrap()`. However, AletheiaDB actually implements `From<u64>` for `NodeId`? No, wait, earlier the compiler error said:
`error[E0277]: the trait bound aletheiadb::NodeId: From<{integer}> is not satisfied`

It seems there is no `From<u64>` implementation for `NodeId` because of the `MAX_VALID_ID` validation! But wait, could it implement `TryFrom<u64>`? Let's check.
