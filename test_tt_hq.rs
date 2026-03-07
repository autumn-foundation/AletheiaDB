use aletheiadb::prelude::*;
use aletheiadb::index::vector::{HnswConfig, DistanceMetric};
use aletheiadb::index::vector::temporal::TemporalVectorConfig;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
let db = AletheiaDB::new().unwrap();

// First, configure and enable the vector index
db.vector_index("embedding")
    .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
    .temporal(TemporalVectorConfig::default())
    .enable()?;

let alice_id = db.create_node("Person", properties! { "name" => "Alice", "age" => 30 })?;
let bob_id = db.create_node("Person", properties! { "name" => "Bob", "age" => 30 })?;
db.create_edge(alice_id, bob_id, "KNOWS", properties! {})?;

// Setup query parameters
let query_embedding = vec![0.1f32; 384];
let valid_time = aletheiadb::time::now();
let tx_time = aletheiadb::time::now();

// Simple: Graph + Vector hybrid
let results = db.traverse_and_rank(alice_id, "KNOWS", &query_embedding, 10)?;
for row in results {
    // Iterate over results (QueryResults is an iterator)
    println!("Found: {:?}", row?.entity);
}

// Complex: Full hybrid with builder
let results = db.query()
    .as_of(valid_time, tx_time)        // Temporal: point-in-time
    .start(alice_id)                   // Graph: start node
    .traverse("KNOWS")                 // Graph: traverse edges
    .rank_by_similarity(&query_embedding, 10) // Vector: rank by similarity
    .with_provenance()                 // Include metadata
    .execute(&db)?;

for row in results {
    // Access score from metadata
    let row = row?;
    if let Some(score) = row.score {
        if score > 0.8 {
            println!("High similarity match: {:?}", row.entity);
        }
    }
}

// Property-specific vector queries
let _results = db.query()
    .find_similar_builder(&query_embedding, 10)
    .property("embedding")  // Query specific property
    .metric(DistanceMetric::Cosine)
    .finish()
    .execute(&db)?;

Ok(())
}
