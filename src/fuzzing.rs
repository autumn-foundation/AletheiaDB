//! Fuzz-only support hooks.
//!
//! This module is compiled only with `--cfg fuzzing` or the internal `fuzzing`
//! feature. It exposes narrow wrappers around parser internals so cargo-fuzz
//! targets can exercise critical storage paths without making those internals
//! part of the normal public API.

use crate::core::hlc::HybridTimestamp;
use crate::core::id::{EdgeId, MAX_VALID_ID, NodeId, VersionId};
use crate::core::temporal::MAX_VALID_TIMESTAMP;
use crate::storage::wal::{LSN, WalEntry};

/// WAL parser and serializer hooks used by fuzz targets.
pub mod wal {
    use super::*;
    use crate::core::error::Result;
    use crate::storage::wal::{segment_reader, serialization};

    /// Parse one WAL entry from `bytes` using the current plaintext WAL version.
    ///
    /// The wrapper intentionally starts at offset zero. Fuzz targets that need to
    /// test segment headers should use the public segment-reader API instead.
    pub fn parse_current_entry(bytes: &[u8]) -> Result<(WalEntry, usize)> {
        segment_reader::parse_entry_at(bytes, 0, segment_reader::WAL_VERSION_PROVENANCE)
    }

    /// Serialize a parsed WAL entry back to bytes with a fresh checksum.
    pub fn serialize_entry(entry: &WalEntry) -> Result<Vec<u8>> {
        let mut buffer = Vec::new();
        serialization::serialize_operation_into(
            entry.lsn,
            entry.timestamp,
            &entry.operation,
            &mut buffer,
        )?;
        Ok(buffer)
    }
}

fn bounded_valid_id(raw: u64) -> u64 {
    raw % MAX_VALID_ID
}

impl<'a> arbitrary::Arbitrary<'a> for LSN {
    fn arbitrary(unstructured: &mut arbitrary::Unstructured<'a>) -> arbitrary::Result<Self> {
        Ok(LSN(u64::arbitrary(unstructured)?))
    }
}

impl<'a> arbitrary::Arbitrary<'a> for NodeId {
    fn arbitrary(unstructured: &mut arbitrary::Unstructured<'a>) -> arbitrary::Result<Self> {
        let raw = bounded_valid_id(u64::arbitrary(unstructured)?);
        Ok(NodeId::new(raw).expect("bounded fuzz node ID must be valid"))
    }
}

impl<'a> arbitrary::Arbitrary<'a> for EdgeId {
    fn arbitrary(unstructured: &mut arbitrary::Unstructured<'a>) -> arbitrary::Result<Self> {
        let raw = bounded_valid_id(u64::arbitrary(unstructured)?);
        Ok(EdgeId::new(raw).expect("bounded fuzz edge ID must be valid"))
    }
}

impl<'a> arbitrary::Arbitrary<'a> for VersionId {
    fn arbitrary(unstructured: &mut arbitrary::Unstructured<'a>) -> arbitrary::Result<Self> {
        let raw = bounded_valid_id(u64::arbitrary(unstructured)?);
        Ok(VersionId::new(raw).expect("bounded fuzz version ID must be valid"))
    }
}

impl<'a> arbitrary::Arbitrary<'a> for HybridTimestamp {
    fn arbitrary(unstructured: &mut arbitrary::Unstructured<'a>) -> arbitrary::Result<Self> {
        let max = MAX_VALID_TIMESTAMP as u64 + 1;
        let wallclock = (u64::arbitrary(unstructured)? % max) as i64;
        let logical = u32::arbitrary(unstructured)?;
        Ok(HybridTimestamp::new(wallclock, logical).expect("bounded fuzz timestamp must be valid"))
    }
}

#[cfg(test)]
mod tests {
    use super::wal::{parse_current_entry, serialize_entry};
    use crate::core::NodeId;
    use crate::core::interning::InternedString;
    use crate::core::property::PropertyMap;
    use crate::core::provenance::Provenance;
    use crate::core::temporal::time;
    use crate::storage::wal::entry::{LSN, WalEntry, WalOperation};

    /// Regression test for a bug where `parse_current_entry` hardcoded the
    /// pre-provenance `WAL_VERSION` (1) even though `serialize_entry` always
    /// writes the provenance presence byte for Create/Update ops (Issue
    /// #3224). Parsing at the stale version left a trailing byte unconsumed,
    /// breaking round-trip byte-count fidelity for every Create/Update op --
    /// exactly the invariant `fuzz/fuzz_targets/wal_entry_parsing.rs` checks.
    #[test]
    fn create_node_roundtrip_consumes_all_bytes() {
        let entry = WalEntry::new(
            LSN(1),
            WalOperation::CreateNode {
                node_id: NodeId::new(1).unwrap(),
                label: InternedString::from_raw(0),
                properties: PropertyMap::new(),
                valid_from: time::now(),
                provenance: Some(
                    Provenance::builder()
                        .source("test")
                        .confidence(0.9)
                        .build()
                        .unwrap(),
                ),
            },
        );

        let canonical = serialize_entry(&entry).expect("entry must serialize");
        let (roundtrip, roundtrip_consumed) =
            parse_current_entry(&canonical).expect("serialized entry must parse");

        assert_eq!(roundtrip_consumed, canonical.len());
        assert_eq!(roundtrip.lsn, entry.lsn);
    }
}
