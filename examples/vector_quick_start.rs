use aletheiadb::{AletheiaDB, properties, HnswConfig, DistanceMetric};
use aletheiadb::index::vector::temporal::TemporalVectorConfig;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = AletheiaDB::new().unwrap();

    // Enable vector indexing with temporal support
    db.vector_index("embedding")
        .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
        .temporal(TemporalVectorConfig::default())
        .enable()?;

    let embedding = vec![0.1f32; 384];

    // Store node with embedding - automatically indexed!
    // Note: We convert the vector to a slice for the properties! macro
    let doc_id = db.create_node("Document",
        properties! {
            "title" => "Introduction to Rust",
            "embedding" => &embedding[..],
        }
    )?;

    // Create another similar node so we have something to find
    let _doc2_id = db.create_node("Document",
        properties! {
            "title" => "Rust for Beginners",
            "embedding" => &embedding[..], // Same embedding for max similarity
        }
    )?;

    // Find similar nodes
    // Note: find_similar excludes the query node itself from results
    let similar = db.find_similar(doc_id, 10)?;

    println!("Similar nodes: {:?}", similar);

    Ok(())
}
