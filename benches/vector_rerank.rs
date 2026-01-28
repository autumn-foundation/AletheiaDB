use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use gallifreydb::core::property::PropertyMapBuilder;
use gallifreydb::index::vector::{DistanceMetric, HnswConfig};
use gallifreydb::query::executor::QueryExecutor;
use gallifreydb::storage::current::CurrentStorage;
use gallifreydb::storage::historical::HistoricalStorage;
use gallifreydb::storage::version::AnchorConfig;
use gallifreydb::{PhysicalOp, PhysicalPlan};
use parking_lot::RwLock;
use std::sync::Arc;

fn create_test_data(count: usize) -> (Arc<CurrentStorage>, Arc<RwLock<HistoricalStorage>>) {
    let current = Arc::new(CurrentStorage::new());
    let historical = Arc::new(RwLock::new(HistoricalStorage::with_config(
        AnchorConfig::default(),
    )));

    // Enable vector index
    current
        .enable_vector_index("embedding", HnswConfig::new(4, DistanceMetric::Cosine))
        .unwrap();

    // Create nodes
    for i in 0..count {
        let embedding = vec![(i % 100) as f32; 4]; // simple embedding
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Node {}", i))
            .insert_vector("embedding", &embedding)
            .build();
        current.create_node("Person", props).unwrap();
    }

    (current, historical)
}

fn bench_vector_rerank(c: &mut Criterion) {
    let mut group = c.benchmark_group("vector_rerank");

    // Test with different node counts
    for count in [1000, 10_000].iter() {
        let (current, historical) = create_test_data(*count);
        let executor = QueryExecutor::new(current.clone(), historical);

        group.throughput(Throughput::Elements(*count as u64));

        group.bench_with_input(BenchmarkId::from_parameter(count), count, |b, &n| {
            b.iter(|| {
                let plan = PhysicalPlan {
                    root: PhysicalOp::VectorRerank {
                        input: Box::new(PhysicalOp::NodeScan {
                            label: Some("Person".to_string()),
                            estimated_rows: n,
                        }),
                        embedding: vec![1.0f32; 4].into(),
                        k: 10,
                        property_key: None,
                    },
                    estimated_cost: Default::default(),
                    temporal_context: None,
                    parallel: false,
                    include_provenance: false,
                };

                let results = executor.execute(plan).expect("Execution failed");
                black_box(results.collect_all().expect("Collection failed"));
            })
        });
    }

    group.finish();
}

criterion_group!(benches, bench_vector_rerank);
criterion_main!(benches);
