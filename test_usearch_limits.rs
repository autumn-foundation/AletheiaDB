use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

fn main() {
    let options = IndexOptions {
        dimensions: 100_001, // Just over limit
        metric: MetricKind::Cos,
        quantization: ScalarKind::F32,
        connectivity: 16,
        expansion_add: 100,
        expansion_search: 100,
        multi: false,
    };

    println!("Attempting to create index with {} dimensions...", options.dimensions);
    match Index::new(&options) {
        Ok(index) => {
            println!("Success! Index created.");
            // Try to save it
            match index.save("high_dim.usearch") {
                Ok(_) => println!("Saved successfully."),
                Err(e) => println!("Save failed: {}", e),
            }
        },
        Err(e) => println!("Failed to create index: {}", e),
    }
}
