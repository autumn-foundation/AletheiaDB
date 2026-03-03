# Test coverage gap analysis:

File: `src/core/property.rs`
Area: Error handling branches during deserialization of `PropertyValue` and `PropertyMap` types.

Gap:
There are several early return branches during the byte-level deserialization of `PropertyValue` and `PropertyMap` that gracefully return `StorageError::CorruptedData`. These checks defend against DoS attacks, OOM, and stack overflows.
Specifically, there are out-of-bounds byte length checks missing coverage during deserialization of Int, Float, String, Bytes, Array, and PropertyMap, as well as bounds checks and utf8 checks.

Solution:
Write a suite of `#[test]` cases that systematically exercise every short-buffer/invalid-data return path in the deserialization functions of `PropertyValue` and `PropertyMap`.

PR Title: 🛡️ Sentry: Property Deserialization Defensive Checks Coverage
Target: `aletheiadb::core::property::{PropertyValue, PropertyMap}::deserialize`
Risk: Ensuring the engine doesn't panic on malformed byte arrays from corrupt storage logs or malicious users.
Strategy: Craft specific byte sequences representing exactly one byte shorter than required, or violating constraints.
Verification: `cargo test --test sentry_property_coverage`
