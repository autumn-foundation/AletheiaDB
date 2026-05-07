
use super::*;

#[test]
fn test_wal_config_defaults() {
    let config = WalConfig::default();
    assert_eq!(config.num_stripes, 16);
    assert_eq!(config.stripe_capacity, 1024);
    assert_eq!(config.write_buffer_size, 64 * 1024);
    assert_eq!(config.segment_size, 64 * 1024 * 1024);
    assert_eq!(config.flush_interval_ms, 10);
}

#[test]
fn test_wal_config_builder() {
    let config = WalConfigBuilder::new()
        .num_stripes(32)
        .unwrap()
        .stripe_capacity(2048)
        .unwrap()
        .write_buffer_size(128 * 1024)
        .unwrap()
        .segment_size(128 * 1024 * 1024)
        .unwrap()
        .flush_interval_ms(20)
        .unwrap()
        .build();

    assert_eq!(config.num_stripes, 32);
    assert_eq!(config.stripe_capacity, 2048);
    assert_eq!(config.write_buffer_size, 128 * 1024);
    assert_eq!(config.segment_size, 128 * 1024 * 1024);
    assert_eq!(config.flush_interval_ms, 20);
}

#[test]
fn test_wal_config_builder_rounds_stripes_to_power_of_two() {
    let config = WalConfigBuilder::new()
        .num_stripes(30)
        .unwrap() // Not a power of 2
        .build();

    assert_eq!(config.num_stripes, 32); // Rounded up to next power of 2
}

#[test]
fn test_wal_config_with_validated() {
    let config = WalConfigBuilder::new()
        .with_validated(
            32,               // num_stripes
            2048,             // stripe_capacity
            128 * 1024,       // write_buffer_size
            64 * 1024 * 1024, // segment_size
            10,               // segments_to_retain
            20,               // flush_interval_ms
        )
        .unwrap() // Single unwrap!
        .build();

    assert_eq!(config.num_stripes, 32);
    assert_eq!(config.stripe_capacity, 2048);
    assert_eq!(config.write_buffer_size, 128 * 1024);
    assert_eq!(config.segment_size, 64 * 1024 * 1024);
    assert_eq!(config.segments_to_retain, 10);
    assert_eq!(config.flush_interval_ms, 20);
}

#[test]
fn test_wal_config_with_validated_invalid() {
    // Test that invalid values are caught
    let result = WalConfigBuilder::new().with_validated(
        0, // invalid: 0 stripes
        2048,
        128 * 1024,
        64 * 1024 * 1024,
        10,
        20,
    );
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
}

#[test]
fn test_historical_config_defaults() {
    let config = HistoricalConfig::default();
    assert_eq!(config.max_versions_per_entity, 1000);
    assert_eq!(config.max_reconstruction_depth, 100);
    assert_eq!(config.reconstruction_cache_size, 10000);
    assert_eq!(config.anchor_interval, 10);
    assert_eq!(config.max_delta_chain, 20);
}

#[test]
fn test_historical_config_builder() {
    let config = HistoricalConfigBuilder::new()
        .max_versions_per_entity(5000)
        .unwrap()
        .max_reconstruction_depth(200)
        .unwrap()
        .reconstruction_cache_size(20000)
        .unwrap()
        .anchor_interval(5)
        .unwrap()
        .max_delta_chain(10)
        .unwrap()
        .build();

    assert_eq!(config.max_versions_per_entity, 5000);
    assert_eq!(config.max_reconstruction_depth, 200);
    assert_eq!(config.reconstruction_cache_size, 20000);
    assert_eq!(config.anchor_interval, 5);
    assert_eq!(config.max_delta_chain, 10);
}

#[test]
fn test_vector_config_defaults() {
    let config = VectorIndexConfig::default();
    assert_eq!(config.max_k, 10000);
    assert_eq!(config.max_layer, 16);
}

#[test]
fn test_vector_config_builder() {
    let config = VectorIndexConfigBuilder::new()
        .max_k(20000)
        .unwrap()
        .max_layer(32)
        .unwrap()
        .build();

    assert_eq!(config.max_k, 20000);
    assert_eq!(config.max_layer, 32);
}

#[test]
fn test_unified_config_defaults() {
    let config = AletheiaDBConfig::default();
    assert_eq!(config.wal, WalConfig::default());
    assert_eq!(config.historical, HistoricalConfig::default());
    assert_eq!(config.vector, VectorIndexConfig::default());
}

#[test]
fn test_unified_config_builder() {
    let config = AletheiaDBConfig::builder()
        .wal(WalConfigBuilder::new().num_stripes(32).unwrap().build())
        .historical(
            HistoricalConfigBuilder::new()
                .max_versions_per_entity(5000)
                .unwrap()
                .build(),
        )
        .vector(
            VectorIndexConfigBuilder::new()
                .max_k(20000)
                .unwrap()
                .build(),
        )
        .build();

    assert_eq!(config.wal.num_stripes, 32);
    assert_eq!(config.historical.max_versions_per_entity, 5000);
    assert_eq!(config.vector.max_k, 20000);
}

#[test]
fn test_wal_config_fluent_api() {
    // Test that builder methods return self for chaining
    let config = WalConfigBuilder::new()
        .num_stripes(8)
        .unwrap()
        .stripe_capacity(512)
        .unwrap()
        .build();

    assert_eq!(config.num_stripes, 8);
    assert_eq!(config.stripe_capacity, 512);
}

#[test]
fn test_embedded_system_config() {
    // Embedded systems need smaller buffers
    let config = AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .num_stripes(4)
                .unwrap()
                .stripe_capacity(256)
                .unwrap()
                .write_buffer_size(16 * 1024)
                .unwrap()
                .segment_size(16 * 1024 * 1024)
                .unwrap()
                .build(),
        )
        .historical(
            HistoricalConfigBuilder::new()
                .max_versions_per_entity(100)
                .unwrap()
                .reconstruction_cache_size(1000)
                .unwrap()
                .build(),
        )
        .build();

    assert_eq!(config.wal.num_stripes, 4);
    assert_eq!(config.wal.write_buffer_size, 16 * 1024);
    assert_eq!(config.historical.max_versions_per_entity, 100);
}

#[test]
fn test_cloud_deployment_config() {
    // Cloud deployments can afford larger capacities
    let config = AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .num_stripes(64)
                .unwrap()
                .stripe_capacity(4096)
                .unwrap()
                .write_buffer_size(256 * 1024)
                .unwrap()
                .segment_size(256 * 1024 * 1024)
                .unwrap()
                .build(),
        )
        .historical(
            HistoricalConfigBuilder::new()
                .max_versions_per_entity(10000)
                .unwrap()
                .reconstruction_cache_size(100000)
                .unwrap()
                .build(),
        )
        .build();

    assert_eq!(config.wal.num_stripes, 64);
    assert_eq!(config.wal.write_buffer_size, 256 * 1024);
    assert_eq!(config.historical.max_versions_per_entity, 10000);
}

#[test]
fn test_batch_processing_config() {
    // Batch processing needs different flush intervals
    let config = AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .flush_interval_ms(100)
                .unwrap() // Longer interval for batching
                .build(),
        )
        .build();

    assert_eq!(config.wal.flush_interval_ms, 100);
}

// TOML configuration tests

#[test]
#[cfg(feature = "config-toml")]
fn test_toml_serialization() {
    let config = AletheiaDBConfig::default();
    let toml_string = config.to_toml_string().unwrap();

    // Should contain all sections
    assert!(toml_string.contains("[wal]"));
    assert!(toml_string.contains("[historical]"));
    assert!(toml_string.contains("[vector]"));

    // Should contain some key values
    assert!(toml_string.contains("num_stripes"));
    assert!(toml_string.contains("max_versions_per_entity"));
    assert!(toml_string.contains("max_k"));
    assert!(toml_string.contains("anchor_interval"));
}

#[test]
#[cfg(feature = "config-toml")]
fn test_toml_deserialization_partial() {
    // Test partial config - only override WAL settings
    let toml_str = r#"
[wal]
num_stripes = 32
stripe_capacity = 2048

[historical]
max_versions_per_entity = 1000
max_reconstruction_depth = 100
reconstruction_cache_size = 10000

[vector]
max_k = 10000
max_layer = 16
        "#;

    let config = AletheiaDBConfig::from_toml_str(toml_str).unwrap();

    assert_eq!(config.wal.num_stripes, 32);
    assert_eq!(config.wal.stripe_capacity, 2048);
    // Other values should have defaults
    assert_eq!(config.wal.write_buffer_size, 64 * 1024);
}

#[test]
#[cfg(feature = "config-toml")]
fn test_toml_deserialization_complete() {
    let toml_str = r#"
[wal]
num_stripes = 32
stripe_capacity = 2048
write_buffer_size = 131072
segment_size = 134217728
flush_interval_ms = 20

[historical]
max_versions_per_entity = 5000
max_reconstruction_depth = 200
reconstruction_cache_size = 20000
anchor_interval = 5
max_delta_chain = 10

[vector]
max_k = 20000
max_layer = 32
        "#;

    let config = AletheiaDBConfig::from_toml_str(toml_str).unwrap();

    // WAL config
    assert_eq!(config.wal.num_stripes, 32);
    assert_eq!(config.wal.stripe_capacity, 2048);
    assert_eq!(config.wal.write_buffer_size, 131072);
    assert_eq!(config.wal.segment_size, 134217728);
    assert_eq!(config.wal.flush_interval_ms, 20);

    // Historical config
    assert_eq!(config.historical.max_versions_per_entity, 5000);
    assert_eq!(config.historical.max_reconstruction_depth, 200);
    assert_eq!(config.historical.reconstruction_cache_size, 20000);
    assert_eq!(config.historical.anchor_interval, 5);
    assert_eq!(config.historical.max_delta_chain, 10);

    // Vector config
    assert_eq!(config.vector.max_k, 20000);
    assert_eq!(config.vector.max_layer, 32);
}

#[test]
#[cfg(feature = "config-toml")]
fn test_toml_round_trip() {
    // Create config with custom values
    let original = AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .num_stripes(32)
                .unwrap()
                .stripe_capacity(2048)
                .unwrap()
                .build(),
        )
        .historical(
            HistoricalConfigBuilder::new()
                .max_versions_per_entity(5000)
                .unwrap()
                .build(),
        )
        .vector(
            VectorIndexConfigBuilder::new()
                .max_k(20000)
                .unwrap()
                .build(),
        )
        .build();

    // Serialize to TOML
    let toml_string = original.to_toml_string().unwrap();

    // Deserialize back
    let deserialized = AletheiaDBConfig::from_toml_str(&toml_string).unwrap();

    // Should be equal
    assert_eq!(original, deserialized);
}

#[test]
#[cfg(feature = "config-toml")]
fn test_toml_file_save_and_load() {
    use tempfile::NamedTempFile;

    let config = AletheiaDBConfig::builder()
        .wal(WalConfigBuilder::new().num_stripes(64).unwrap().build())
        .build();

    // Create a temporary file
    let temp_file = NamedTempFile::new().unwrap();
    let path = temp_file.path();

    // Save to file
    config.to_toml_file(path).unwrap();

    // Load from file
    let loaded = AletheiaDBConfig::from_toml_file(path).unwrap();

    // Should be equal
    assert_eq!(config, loaded);
}

#[test]
#[cfg(feature = "config-toml")]
fn test_toml_embedded_system_example() {
    let toml_str = r#"
# Embedded system configuration
[wal]
num_stripes = 4
stripe_capacity = 256
write_buffer_size = 16384
segment_size = 16777216

[historical]
max_versions_per_entity = 100
max_reconstruction_depth = 50
reconstruction_cache_size = 1000

[vector]
max_k = 1000
max_layer = 8
        "#;

    let config = AletheiaDBConfig::from_toml_str(toml_str).unwrap();

    assert_eq!(config.wal.num_stripes, 4);
    assert_eq!(config.wal.stripe_capacity, 256);
    assert_eq!(config.historical.max_versions_per_entity, 100);
    assert_eq!(config.vector.max_k, 1000);
}

#[test]
#[cfg(feature = "config-toml")]
fn test_toml_cloud_deployment_example() {
    let toml_str = r#"
# Cloud deployment configuration
[wal]
num_stripes = 64
stripe_capacity = 4096
write_buffer_size = 262144
segment_size = 268435456

[historical]
max_versions_per_entity = 10000
max_reconstruction_depth = 200
reconstruction_cache_size = 100000

[vector]
max_k = 50000
max_layer = 24
        "#;

    let config = AletheiaDBConfig::from_toml_str(toml_str).unwrap();

    assert_eq!(config.wal.num_stripes, 64);
    assert_eq!(config.wal.stripe_capacity, 4096);
    assert_eq!(config.historical.max_versions_per_entity, 10000);
    assert_eq!(config.vector.max_k, 50000);
}

#[test]
#[cfg(feature = "config-toml")]
fn test_toml_parse_error() {
    let invalid_toml = "this is not valid toml {]";
    let result = AletheiaDBConfig::from_toml_str(invalid_toml);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), ConfigError::ParseError(_)));
}

#[test]
#[cfg(feature = "config-toml")]
fn test_toml_durability_mode_group_commit() {
    let toml_str = r#"
[wal]
num_stripes = 32

[wal.durability_mode.GroupCommit]
max_delay_ms = 10
max_batch_size = 200
        "#;
    let config = AletheiaDBConfig::from_toml_str(toml_str).unwrap();
    assert_eq!(config.wal.num_stripes, 32);
    match config.wal.durability_mode {
        crate::storage::wal::DurabilityMode::GroupCommit {
            max_delay_ms,
            max_batch_size,
        } => {
            assert_eq!(max_delay_ms, 10);
            assert_eq!(max_batch_size, 200);
        }
        _ => panic!("Expected GroupCommit durability mode"),
    }
}

#[test]
#[cfg(feature = "config-toml")]
fn test_toml_durability_mode_async() {
    let toml_str = r#"
[wal]
[wal.durability_mode.Async]
flush_interval_ms = 100
        "#;
    let config = AletheiaDBConfig::from_toml_str(toml_str).unwrap();
    match config.wal.durability_mode {
        crate::storage::wal::DurabilityMode::Async { flush_interval_ms } => {
            assert_eq!(flush_interval_ms, 100);
        }
        _ => panic!("Expected Async durability mode"),
    }
}

#[test]
#[cfg(feature = "config-toml")]
fn test_toml_wal_dir() {
    use std::path::PathBuf;
    let toml_str = r#"
[wal]
wal_dir = "/custom/path/to/wal"
        "#;
    let config = AletheiaDBConfig::from_toml_str(toml_str).unwrap();
    assert_eq!(config.wal.wal_dir, PathBuf::from("/custom/path/to/wal"));
}

// Validation error tests

#[test]
fn test_wal_config_zero_num_stripes() {
    let result = WalConfigBuilder::new().num_stripes(0);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
}

#[test]
fn test_wal_config_zero_stripe_capacity() {
    let result = WalConfigBuilder::new().stripe_capacity(0);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
}

#[test]
fn test_wal_config_zero_write_buffer_size() {
    let result = WalConfigBuilder::new().write_buffer_size(0);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
}

#[test]
fn test_wal_config_zero_segment_size() {
    let result = WalConfigBuilder::new().segment_size(0);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
}

#[test]
fn test_wal_config_segment_size_too_small() {
    let result = WalConfigBuilder::new().segment_size(256); // Less than 512 bytes
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
}

#[test]
fn test_wal_config_zero_segments_to_retain() {
    let result = WalConfigBuilder::new().segments_to_retain(0);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
}

#[test]
fn test_wal_config_zero_flush_interval() {
    let result = WalConfigBuilder::new().flush_interval_ms(0);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
}

#[test]
fn test_historical_config_zero_max_versions() {
    let result = HistoricalConfigBuilder::new().max_versions_per_entity(0);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
}

#[test]
fn test_historical_config_zero_max_reconstruction_depth() {
    let result = HistoricalConfigBuilder::new().max_reconstruction_depth(0);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
}

#[test]
fn test_historical_config_max_reconstruction_depth_too_large() {
    let result = HistoricalConfigBuilder::new().max_reconstruction_depth(2000);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
}

#[test]
fn test_historical_config_zero_cache_size() {
    let result = HistoricalConfigBuilder::new().reconstruction_cache_size(0);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
}

#[test]
fn test_historical_config_zero_anchor_interval() {
    let result = HistoricalConfigBuilder::new().anchor_interval(0);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
}

#[test]
fn test_historical_config_zero_max_delta_chain() {
    let result = HistoricalConfigBuilder::new().max_delta_chain(0);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
}

#[test]
fn test_historical_config_build_checked_cold_storage_missing_path() {
    let result = HistoricalConfigBuilder::new()
        .enable_cold_storage(true)
        .build_checked();
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
}

#[test]
fn test_historical_config_build_checked_cold_storage_valid_path() {
    use std::path::PathBuf;
    let result = HistoricalConfigBuilder::new()
        .enable_cold_storage(true)
        .cold_storage_path(PathBuf::from("/tmp/test"))
        .build_checked();
    assert!(result.is_ok());
}

#[test]
fn test_vector_config_zero_max_k() {
    let result = VectorIndexConfigBuilder::new().max_k(0);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
}

#[test]
fn test_vector_config_max_k_too_large() {
    let result = VectorIndexConfigBuilder::new().max_k(200_000);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
}

#[test]
fn test_vector_config_zero_max_layer() {
    let result = VectorIndexConfigBuilder::new().max_layer(0);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
}

#[test]
fn test_vector_config_max_layer_too_large() {
    let result = VectorIndexConfigBuilder::new().max_layer(64);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), ConfigError::InvalidValue(_)));
}
