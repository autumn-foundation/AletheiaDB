## 2024-05-18 - Eradicate unwrap() panic hazards in deserialization
**The Trigger:** Malformed byte lengths in `try_into().unwrap()` paths can cause hard panics instead of graceful error routing.
**The Stack Trace:** Panic at `.unwrap()` on Result across `src/core/property/value.rs`, `src/core/vector/serialization.rs`, etc.
**Reproduction:** Create a `proptest` suite in `tests/havoc/havoc_try_into_panics.rs` to generate random/short byte vectors.
**Comment:** We swapped `.try_into().unwrap()` to `.unwrap_or_default()` in byte array conversions for `from_le_bytes` functions. Since these are all preceded by bounds length checks `if bytes.len() < X`, `try_into()` handles the conversion safely and the `unwrap_or_default()` behaves identically (returning `0`s safely if a length check drifted) without panicking.
