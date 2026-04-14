//! Additional tests for vector mod.rs code coverage.

use aletheiadb::index::vector::{
    CustomMetric, DistanceMetric, Quantization, StorageMode, TemporalSearchResults,
};
use std::path::PathBuf;
use std::sync::Arc;

// ============================================================================
// CustomMetric tests
// ============================================================================

#[test]
fn test_custom_metric_clone() {
    let metric = CustomMetric {
        name: "test_metric".to_string(),
        distance_fn: Arc::new(|a: &[f32], b: &[f32]| {
            a.iter().zip(b.iter()).map(|(x, y)| (x - y).abs()).sum()
        }),
    };

    let cloned = metric.clone();

    assert_eq!(cloned.name, "test_metric");

    // Verify the cloned function works
    let result = (cloned.distance_fn)(&[1.0, 2.0], &[0.0, 0.0]);
    assert!((result - 3.0).abs() < 0.001);
}

#[test]
fn test_custom_metric_debug() {
    let metric = CustomMetric {
        name: "debug_test".to_string(),
        distance_fn: Arc::new(|_, _| 0.0),
    };

    let debug_str = format!("{:?}", metric);
    assert!(debug_str.contains("CustomMetric"));
    assert!(debug_str.contains("debug_test"));
    assert!(debug_str.contains("<function>"));
}

#[test]
fn test_custom_metric_partial_eq() {
    let metric1 = CustomMetric {
        name: "same_name".to_string(),
        distance_fn: Arc::new(|_, _| 0.0),
    };

    let metric2 = CustomMetric {
        name: "same_name".to_string(),
        distance_fn: Arc::new(|_, _| 1.0), // Different function, same name
    };

    let metric3 = CustomMetric {
        name: "different_name".to_string(),
        distance_fn: Arc::new(|_, _| 0.0),
    };

    // Should be equal by name
    assert_eq!(metric1, metric2);
    // Different names should not be equal
    assert_ne!(metric1, metric3);
}

// ============================================================================
// Quantization tests
// ============================================================================

#[test]
fn test_quantization_debug() {
    assert_eq!(format!("{:?}", Quantization::F32), "F32");
    assert_eq!(format!("{:?}", Quantization::F16), "F16");
    assert_eq!(format!("{:?}", Quantization::I8), "I8");
}

#[test]
fn test_quantization_clone() {
    let q = Quantization::F16;
    #[allow(clippy::clone_on_copy)]
    let cloned = q.clone();
    assert_eq!(q, cloned);
}

#[test]
fn test_quantization_copy() {
    let q1 = Quantization::I8;
    let q2 = q1; // Copy
    assert_eq!(q1, q2);
}

// ============================================================================
// StorageMode tests
// ============================================================================

#[test]
fn test_storage_mode_debug() {
    let in_memory = StorageMode::InMemory;
    assert!(format!("{:?}", in_memory).contains("InMemory"));

    let mmap = StorageMode::MemoryMapped {
        path: PathBuf::from("/test/path"),
    };
    let debug_str = format!("{:?}", mmap);
    assert!(debug_str.contains("MemoryMapped"));
    assert!(debug_str.contains("/test/path") || debug_str.contains("\\test\\path"));
}

#[test]
fn test_storage_mode_clone() {
    let mmap = StorageMode::MemoryMapped {
        path: PathBuf::from("/test/path"),
    };
    let cloned = mmap.clone();

    match cloned {
        StorageMode::MemoryMapped { path } => {
            assert_eq!(path, PathBuf::from("/test/path"));
        }
        _ => panic!("Expected MemoryMapped"),
    }
}

#[test]
fn test_storage_mode_partial_eq() {
    let in_memory1 = StorageMode::InMemory;
    let in_memory2 = StorageMode::InMemory;
    assert_eq!(in_memory1, in_memory2);

    let mmap1 = StorageMode::MemoryMapped {
        path: PathBuf::from("/path1"),
    };
    let mmap2 = StorageMode::MemoryMapped {
        path: PathBuf::from("/path1"),
    };
    let mmap3 = StorageMode::MemoryMapped {
        path: PathBuf::from("/path2"),
    };

    assert_eq!(mmap1, mmap2);
    assert_ne!(mmap1, mmap3);
    assert_ne!(in_memory1, mmap1);
}

// ============================================================================
// DistanceMetric tests for new variants
// ============================================================================

#[test]
fn test_distance_metric_haversine_roundtrip() {
    let metric = DistanceMetric::Haversine;
    let byte = metric.to_u8();
    let restored = DistanceMetric::from_u8(byte).unwrap();
    assert_eq!(metric, restored);
}

#[test]
fn test_distance_metric_hamming_roundtrip() {
    let metric = DistanceMetric::Hamming;
    let byte = metric.to_u8();
    let restored = DistanceMetric::from_u8(byte).unwrap();
    assert_eq!(metric, restored);
}

#[test]
fn test_distance_metric_tanimoto_roundtrip() {
    let metric = DistanceMetric::Tanimoto;
    let byte = metric.to_u8();
    let restored = DistanceMetric::from_u8(byte).unwrap();
    assert_eq!(metric, restored);
}

#[test]
fn test_distance_metric_all_values() {
    // Verify all byte values decode correctly
    for i in 0..=5 {
        let result = DistanceMetric::from_u8(i);
        assert!(result.is_ok());
    }

    // Values >= 6 should fail
    assert!(DistanceMetric::from_u8(6).is_err());
    assert!(DistanceMetric::from_u8(255).is_err());
}

#[test]
fn test_distance_metric_debug() {
    assert_eq!(format!("{:?}", DistanceMetric::Haversine), "Haversine");
    assert_eq!(format!("{:?}", DistanceMetric::Hamming), "Hamming");
    assert_eq!(format!("{:?}", DistanceMetric::Tanimoto), "Tanimoto");
}

#[test]
fn test_distance_metric_clone_copy() {
    let m1 = DistanceMetric::Haversine;
    let m2 = m1; // Copy
    #[allow(clippy::clone_on_copy)]
    let m3 = m1.clone();
    assert_eq!(m1, m2);
    assert_eq!(m1, m3);
}

// ============================================================================
// TemporalSearchResults type alias test
// ============================================================================

#[test]
fn test_temporal_search_results_type() {
    use aletheiadb::core::id::NodeId;

    // Verify the type alias works correctly
    // Timestamp is a type alias for i64 (milliseconds since epoch)
    let results: TemporalSearchResults = vec![
        (1000i64.into(), vec![(NodeId::new(1).unwrap(), 0.9)]),
        (2000i64.into(), vec![(NodeId::new(2).unwrap(), 0.8)]),
    ];

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].1.len(), 1);
}

// ============================================================================
// Error message tests
// ============================================================================

#[test]
fn test_invalid_metric_error_message() {
    let result = DistanceMetric::from_u8(99);
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("99") || err_msg.contains("Invalid"));
}
