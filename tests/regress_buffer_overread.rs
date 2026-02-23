
use aletheiadb::index::vector::{HnswIndexBuilder, DistanceMetric, Quantization, VectorIndex, HnswConfig, HnswIndex};
use aletheiadb::core::id::NodeId;
use std::sync::{Arc, Mutex};
use std::fs;

#[test]
fn test_regress_buffer_overread() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let f32_path = dir.path().join("f32.index");
    let f16_path = dir.path().join("f16.index");

    // 1. Create F32 index template
    {
        let index = HnswIndexBuilder::new(16, DistanceMetric::Cosine)
            .quantization(Quantization::F32)
            .build()?;

        // Add some vectors
        for i in 0..100 {
            let vec = vec![1.0f32; 16];
            index.add(NodeId::new(i).unwrap(), &vec)?;
        }
        index.save(&f32_path)?;
    }

    // 2. Create F16 index (same content)
    {
        let index = HnswIndexBuilder::new(16, DistanceMetric::Cosine)
            .quantization(Quantization::F16)
            .build()?;

        for i in 0..100 {
            let vec = vec![1.0f32; 16];
            index.add(NodeId::new(i).unwrap(), &vec)?;
        }
        index.save(&f16_path)?;
    }

    // 3. The Attack: Overwrite F32 index file with F16 index file
    // BUT keep the F32 mappings file!
    fs::copy(&f16_path, &f32_path)?;
    // We do NOT copy the mappings file, so f32.usearch.mappings remains F32.

    // Shared state to detect anomaly (in case verify fails to catch it)
    let saw_anomaly = Arc::new(Mutex::new(false));
    let anomaly_clone = saw_anomaly.clone();

    // 4. Load the "F32" index (which is now F16 binary + F32 mappings)
    println!("Loading corrupted index...");

    // This config matches the mappings file (F32), so validate_metadata passes.
    let config = HnswConfig::new(16, DistanceMetric::Cosine)
        .with_quantization(Quantization::F32)
        .with_custom_metric("test", move |a, _b| {
            let sum_a: f32 = a.iter().sum();

            // Expected sum for 16 * 1.0 is 16.0.
            if (sum_a - 16.0).abs() > 1.0 {
                // Anomaly detected! We are reading garbage.
                *anomaly_clone.lock().unwrap() = true;
            }
            0.0
        });

    let result = HnswIndex::load(&f32_path, config);

    // 5. Verify the fix: Load MUST fail with header mismatch error
    match result {
        Ok(_) => {
            // If load succeeded, check if we actually have the vulnerability
            // (Only if we proceed to search, but here we just check load)
            panic!("Index loaded successfully! Vulnerability not prevented. Expected header mismatch error.");
        }
        Err(e) => {
            let msg = e.to_string();
            println!("Got expected error: {}", msg);
            assert!(msg.contains("Index file header mismatch"));
            assert!(msg.contains("expected 64 bytes"));
            assert!(msg.contains("found 32"));
        }
    }

    Ok(())
}
