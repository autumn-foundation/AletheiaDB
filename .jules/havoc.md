**[Parser DoS Proptests]**
**The Trigger:** Inputs like random strings with random control characters, valid and invalid unicode scalars.
**The Stack Trace:** None, tests pass.
**Reproduction:** Run `cargo test --test havoc fuzz_aql_parser`.
**Comment:** Looks like the parser handles random noise appropriately by returning errors without panicking.

**[Flush Coordinator Unsafe from_bytes]**
**The Trigger:** Arbitrary bytes from file deserialized via memory mapping could be passed to `SegmentMetadata::from_bytes`.
**The Stack Trace:** None, tests pass safely.
**Reproduction:** Run `cargo test --test havoc test_havoc_flush_coordinator_from_bytes`.
**Comment:** The explicit length checks ensure safe decoding without hitting OOB panics.

**[BatchBuffer Loom Torture]**
**The Trigger:** Concurrent calls to `add` and `flush` on `BatchBuffer`.
**The Stack Trace:** None.
**Reproduction:** Run `RUSTFLAGS="--cfg loom" cargo test --test havoc --features "honeycomb-client"`.
**Comment:** AletheiaDB developers correctly ordered their mutexes. Boring.
