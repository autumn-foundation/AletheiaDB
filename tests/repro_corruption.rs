#[cfg(test)]
mod tests {
    use aletheiadb::storage::redb_cold_storage::{encode_node_version};
    use aletheiadb::core::version::NodeVersion;
    use aletheiadb::core::id::{VersionId, NodeId};
    use aletheiadb::core::temporal::BiTemporalInterval;
    use aletheiadb::core::property::PropertyMapBuilder;
    use aletheiadb::core::interning::{InternedString};

    #[test]
    fn test_encode_node_version_fails_on_invalid_id() {
        // create a forged ID that definitely doesn't exist
        // We assume 1 billion is safe and unused
        let invalid_id = InternedString::from_raw(1_000_000_000);

        let props = PropertyMapBuilder::new().build();
        let version = NodeVersion::new_anchor(
            VersionId::new(1).unwrap(),
            NodeId::new(1).unwrap(),
            BiTemporalInterval::current(1000.into()),
            invalid_id, // This ID does not exist in GLOBAL_INTERNER
            props,
        );

        // Now this should fail with an error
        let result = encode_node_version(&version);

        assert!(result.is_err(), "encode_node_version should fail for invalid ID");
        let err = result.unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("Failed to resolve interned label ID"), "Error message should mention resolution failure: {}", msg);

        println!("Confirmed: Invalid ID caused explicit error as expected.");
    }
}
