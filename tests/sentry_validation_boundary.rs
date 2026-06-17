use aletheiadb::AletheiaDB;
use aletheiadb::api::transaction::WriteOps;
use aletheiadb::core::hlc::HybridTimestamp;
use aletheiadb::core::property::PropertyMap;
use aletheiadb::core::temporal::time;
use std::sync::Arc;

#[test]
fn test_sentry_valid_time_future_validation_bounds() {
    let db = Arc::new(AletheiaDB::new().unwrap());

    let max_valid_time_future_offset_us: i64 = 365 * 24 * 60 * 60 * 1_000_000;

    let near_limit_wallclock =
        time::now().wallclock() + max_valid_time_future_offset_us - 1_000_000;
    let near_limit_ts = HybridTimestamp::new(near_limit_wallclock, 0).unwrap();

    let result = db.write(|tx| {
        tx.create_node_with_valid_time("Test1", PropertyMap::new(), Some(near_limit_ts))?;
        Ok::<_, aletheiadb::core::error::Error>(())
    });

    assert!(
        result.is_ok(),
        "Should accept valid_time below the 1-year limit"
    );

    let over_limit_wallclock =
        time::now().wallclock() + max_valid_time_future_offset_us + 1_000_000;
    let over_limit_ts = HybridTimestamp::new(over_limit_wallclock, 0).unwrap();

    let result2 = db.write(|tx| {
        tx.create_node_with_valid_time("Test2", PropertyMap::new(), Some(over_limit_ts))?;
        Ok::<_, aletheiadb::core::error::Error>(())
    });

    assert!(
        result2.is_err(),
        "Should reject valid_time beyond the limit"
    );
}

#[test]
fn test_sentry_valid_time_creation_bounds() {
    let db = Arc::new(AletheiaDB::new().unwrap());

    // Test before creation
    let result3 = db.write(|tx| {
        let node_id = tx.create_node("NodeForCreation", PropertyMap::new())?;

        let before_creation_ts = HybridTimestamp::new(100, 0).unwrap();

        tx.update_node_with_valid_time(node_id, PropertyMap::new(), Some(before_creation_ts))?;
        Ok::<_, aletheiadb::core::error::Error>(())
    });

    assert!(result3.is_err(), "Should reject update before creation");
}
