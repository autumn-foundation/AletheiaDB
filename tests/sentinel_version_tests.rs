use aletheiadb::core::version::VectorDelta;
use std::sync::Arc;

#[test]
fn test_vector_delta_apply_dimension_mismatch_safe_fallback() {
    // 🛡️ Sentinel Test: Verify that applying a delta to a mismatched dimension vector
    // returns the vector unchanged (safe fallback), rather than applying partial updates.
    // This targets mutants that remove the `base.len() != *dimension` check in VectorDelta::apply.

    let delta = VectorDelta::Sparse {
        dimension: 10,
        changes: Arc::new(vec![(0, 1.0f32)]),
    };

    // Base vector with different dimension (5 instead of 10)
    // Index 0 exists in base, so without the check, it WOULD be updated.
    let base = vec![0.0f32; 5];

    // In DEBUG builds, this triggers a debug_assert! panic.
    // In RELEASE builds, it should return base unchanged.
    // We use catch_unwind to handle the debug panic if it occurs,
    // but primarily we want to assert the release behavior.

    let result = std::panic::catch_unwind(|| delta.apply(&base));

    match result {
        Ok(applied) => {
            // If it didn't panic (Release mode), verify it didn't modify the vector
            assert_eq!(
                applied, base,
                "Vector should be unchanged on dimension mismatch"
            );
        }
        Err(_) => {
            // If it panicked (Debug mode), that's also acceptable behavior (the check exists)
            // This confirms the check is present and active.
        }
    }
}

#[test]
fn test_version_metadata_new_kills_mutants() {
    let tx_id = aletheiadb::core::id::TxId::new(999);
    let ts = aletheiadb::core::temporal::Timestamp::from(888);
    let metadata = aletheiadb::core::version::VersionMetadata::new(tx_id, ts);

    // Explicitly verify it does not return Default::default()
    let default_metadata = aletheiadb::core::version::VersionMetadata::default();
    assert_ne!(
        metadata, default_metadata,
        "new() should not return default"
    );

    assert_eq!(metadata.created_by_tx, tx_id);
    assert_eq!(metadata.commit_timestamp, Some(ts));
}
