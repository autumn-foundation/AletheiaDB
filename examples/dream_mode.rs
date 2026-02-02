//! Example: Gallifrey Dream Mode
//!
//! Run with: cargo run --example dream_mode --features nova
//!
//! This example creates a database, seeds it with random vectors (simulating
//! a knowledge graph), and then launches the "Dream Mode" visualization.

use gallifreydb::GallifreyDB;
use gallifreydb::HnswConfig;
use gallifreydb::PropertyMapBuilder;
#[cfg(feature = "nova")]
use gallifreydb::experimental::dream::{DreamConfig, DreamEngine};
use gallifreydb::index::vector::DistanceMetric;
use rand::Rng;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(not(feature = "nova"))]
    {
        println!("This example requires the 'nova' feature.");
        println!("Run with: cargo run --example dream_mode --features nova");
        return Ok(());
    }

    #[cfg(feature = "nova")]
    run_dream()
}

#[cfg(feature = "nova")]
fn run_dream() -> Result<(), Box<dyn std::error::Error>> {
    println!("Initializing Gallifrey Dream Mode...");
    // Use a temp dir to avoid polluting project root and to ensure fresh start
    let temp_dir = tempfile::tempdir()?;

    // Configure WAL to use the temp directory
    let wal_config = gallifreydb::config::WalConfigBuilder::new()
        .wal_dir(temp_dir.path().to_path_buf())
        .build();

    let db = GallifreyDB::with_wal_config(wal_config)?;

    // Enable vector index
    let dim = 128; // Smaller dimension for demo speed
    let config = HnswConfig::new(dim, DistanceMetric::Cosine);
    db.enable_vector_index("embedding", config)?;

    println!("Seeding thought cloud with 100 random vectors...");
    let mut rng = rand::thread_rng();

    // Create 100 random nodes
    for i in 0..100 {
        let mut vector = vec![0.0; dim];
        for x in &mut vector {
            *x = rng.gen_range(-1.0..1.0);
        }
        // Normalize
        let mag: f32 = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        if mag > 1e-6 {
            for x in &mut vector {
                *x /= mag;
            }
        }

        let props = PropertyMapBuilder::new()
            .insert("name", format!("Thought #{}", i))
            .insert_vector("embedding", &vector)
            .build();

        let _ = db.create_node("Thought", props)?;
    }

    println!("Entering Dream State... (Press 'q' to quit)");
    // Small delay to read message
    std::thread::sleep(std::time::Duration::from_secs(2));

    let dream_config = DreamConfig {
        dimensions: dim,
        noise_scale: 0.1,
        attraction_weight: 0.2,
        friction: 0.95,
        history_limit: 50,
    };

    let mut engine = DreamEngine::new(&db, dream_config);

    // Seed the dream with a random vector
    let mut seed_vec = vec![0.0; dim];
    for x in &mut seed_vec {
        *x = rng.gen_range(-1.0..1.0);
    }
    engine.seed(seed_vec);

    engine.run_tui()?;

    Ok(())
}
