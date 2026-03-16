use aletheiadb::config::{
    HistoricalConfigBuilder, VectorIndexConfigBuilder, WalConfigBuilder,
    AletheiaDBConfigBuilder, AletheiaDBConfig
};
use aletheiadb::DurabilityMode;

use std::time::Duration;
use std::path::Path;

#[test]
fn test_historical_config_builder_methods_exact() {
    let builder = HistoricalConfigBuilder::new();
    let builder = builder.max_versions_per_entity(100).unwrap();
    let builder = builder.max_reconstruction_depth(50).unwrap();
    let builder = builder.reconstruction_cache_size(5000).unwrap();
    let builder = builder.anchor_interval(5).unwrap();
    let builder = builder.max_delta_chain(10).unwrap();
    let builder = builder.enable_cold_storage(true);
    let builder = builder.cold_storage_path("test_path");
    let builder = builder.migration_age_threshold(Duration::from_secs(1000));
    let builder = builder.max_hot_versions(50);
    let config = builder.build();
    assert_eq!(config.max_versions_per_entity, 100);
    assert_eq!(config.max_reconstruction_depth, 50);
    assert_eq!(config.reconstruction_cache_size, 5000);
    assert_eq!(config.anchor_interval, 5);
    assert_eq!(config.max_delta_chain, 10);
    assert!(config.enable_cold_storage);
    assert_eq!(config.cold_storage_path.as_deref(), Some(Path::new("test_path")));
    assert_eq!(config.migration_age_threshold, Duration::from_secs(1000));
    assert_eq!(config.max_hot_versions, 50);
}

#[test]
fn test_vector_config_builder_methods_exact() {
    let builder = VectorIndexConfigBuilder::new();
    let builder = builder.max_k(20).unwrap();
    let builder = builder.max_layer(2).unwrap();
    let config = builder.build();
    assert_eq!(config.max_k, 20);
    assert_eq!(config.max_layer, 2);
}

#[test]
fn test_wal_config_builder_methods_exact() {
    let builder = WalConfigBuilder::new();
    let builder = builder.num_stripes(8).unwrap();
    let builder = builder.stripe_capacity(512).unwrap();
    let builder = builder.write_buffer_size(32768).unwrap();
    let builder = builder.segment_size(16777216).unwrap();
    let builder = builder.segments_to_retain(3).unwrap();
    let builder = builder.flush_interval_ms(5).unwrap();
    let builder = builder.durability_mode(DurabilityMode::AsyncBatched { max_delay_ms: 50, max_batch_size: 100 });
    let builder = builder.wal_dir("test_dir".into());
    let config = builder.build();

    assert_eq!(config.num_stripes, 8);
    assert_eq!(config.stripe_capacity, 512);
    assert_eq!(config.write_buffer_size, 32768);
    assert_eq!(config.segment_size, 16777216);
    assert_eq!(config.segments_to_retain, 3);
    assert_eq!(config.flush_interval_ms, 5);
    assert!(matches!(config.durability_mode, DurabilityMode::AsyncBatched { max_delay_ms: 50, max_batch_size: 100 }));
    assert_eq!(config.wal_dir.to_str().unwrap(), "test_dir");
}

#[test]
fn test_aletheiadb_config_builder_methods_exact() {
    let builder = AletheiaDBConfigBuilder::new();
    let wal = WalConfigBuilder::new().build();
    let hist = HistoricalConfigBuilder::new().build();
    let vec = VectorIndexConfigBuilder::new().build();

    let builder = builder.wal(wal.clone());
    let builder = builder.historical(hist.clone());
    let builder = builder.vector(vec.clone());
    let config = builder.build();

    assert_eq!(config.wal.num_stripes, wal.num_stripes);
    assert_eq!(config.historical.max_versions_per_entity, hist.max_versions_per_entity);
    assert_eq!(config.vector.max_k, vec.max_k);
}

#[test]
fn test_aletheiadb_config_from_toml_str_exact() {
    let toml_str = r#"
[wal]
num_stripes = 4
stripe_capacity = 256
write_buffer_size = 16384
segment_size = 8388608
flush_interval_ms = 2
segments_to_retain = 2
wal_dir = "my_wal"

[historical]
max_versions_per_entity = 500
max_reconstruction_depth = 20
reconstruction_cache_size = 2000
anchor_interval = 2
max_delta_chain = 4
enable_cold_storage = false

[vector]
max_k = 10
max_layer = 1
"#;
    let config = AletheiaDBConfig::from_toml_str(toml_str).unwrap();
    assert_eq!(config.wal.num_stripes, 4);
    assert_eq!(config.historical.max_versions_per_entity, 500);
    assert_eq!(config.vector.max_k, 10);
}

#[test]
fn test_aletheiadb_config_to_toml_string_exact() {
    let config = AletheiaDBConfig::default();
    let toml_str = config.to_toml_string().unwrap();
    assert!(toml_str.contains("num_stripes"));
    assert!(toml_str.contains("max_versions_per_entity"));
    assert!(toml_str.contains("max_k"));
}

#[test]
fn test_aletheiadb_config_from_toml_file_exact() {
    let toml_str = r#"
[wal]
num_stripes = 4
stripe_capacity = 256
write_buffer_size = 16384
segment_size = 8388608
flush_interval_ms = 2
segments_to_retain = 2
wal_dir = "my_wal"

[historical]
max_versions_per_entity = 500
max_reconstruction_depth = 20
reconstruction_cache_size = 2000
anchor_interval = 2
max_delta_chain = 4
enable_cold_storage = false

[vector]
max_k = 10
max_layer = 1
"#;
    let temp_dir = tempfile::tempdir().unwrap();
    let file_path = temp_dir.path().join("config.toml");
    std::fs::write(&file_path, toml_str).unwrap();

    let config = AletheiaDBConfig::from_toml_file(&file_path).unwrap();
    assert_eq!(config.wal.num_stripes, 4);
    assert_eq!(config.historical.max_versions_per_entity, 500);
    assert_eq!(config.vector.max_k, 10);

    let out_file_path = temp_dir.path().join("out_config.toml");
    config.to_toml_file(&out_file_path).unwrap();
    let read_back_str = std::fs::read_to_string(&out_file_path).unwrap();
    assert!(read_back_str.contains("num_stripes"));
    assert!(read_back_str.contains("max_versions_per_entity"));
    assert!(read_back_str.contains("max_k"));
}
