#![no_main]

mod support;

use aletheiadb::PropertyMapBuilder;
use aletheiadb::core::id::{NodeId, VersionId};
use aletheiadb::storage::{AnchorConfig, HistoricalStorage};
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use support::{MAX_FUZZ_OPS, intern_label, timestamp_from_step};

#[derive(Debug, Arbitrary)]
struct TemporalReconstructionCase {
    anchor_interval: u8,
    values: Vec<i64>,
}

fuzz_target!(|case: TemporalReconstructionCase| {
    let anchor_interval = u32::from(case.anchor_interval % 8) + 2;
    let config = AnchorConfig {
        anchor_interval,
        max_delta_chain: anchor_interval * 2,
    };
    let mut historical = HistoricalStorage::with_config(config);
    let node_id = NodeId::new(1).expect("static fuzz node ID must be valid");
    let label = intern_label("FuzzNode");

    for (idx, value) in case.values.iter().copied().take(MAX_FUZZ_OPS).enumerate() {
        let version_id =
            VersionId::new((idx as u64) + 1).expect("bounded fuzz version ID must be valid");
        let valid_time = timestamp_from_step(idx as u64 + 1);
        let tx_time = timestamp_from_step(idx as u64 + 10_000);
        let properties = PropertyMapBuilder::new().insert("value", value).build();

        historical
            .add_node_version(
                node_id, version_id, valid_time, tx_time, label, properties, false,
            )
            .expect("valid fuzz node version must be stored");

        let reconstructed = historical
            .reconstruct_node_properties(version_id)
            .expect("stored fuzz version must reconstruct");
        assert_eq!(
            reconstructed
                .get("value")
                .and_then(|stored| stored.as_int()),
            Some(value)
        );

        assert_eq!(
            historical.find_node_version_at_time(node_id, valid_time, tx_time),
            Some(version_id)
        );
    }
});
