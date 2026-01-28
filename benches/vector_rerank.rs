use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use gallifreydb::core::property::{PropertyMapBuilder, PropertyValue};
use gallifreydb::core::vector::cosine_similarity;
use gallifreydb::index::vector::{DistanceMetric, HnswConfig};
use gallifreydb::query::executor::QueryExecutor;
use gallifreydb::query::executor::{QueryRow, ResultIterator};
use gallifreydb::storage::current::CurrentStorage;
use gallifreydb::storage::historical::HistoricalStorage;
use gallifreydb::storage::version::AnchorConfig;
use gallifreydb::{PhysicalOp, PhysicalPlan};
use parking_lot::RwLock;
use std::sync::Arc;

/// A baseline implementation of VectorRerankIterator that uses the old logic (Vec::new() + sort).
/// This allows us to compare the performance improvement directly in the benchmark.
struct BaselineVectorRerankIterator {
    sorted: Option<std::vec::IntoIter<(QueryRow, f32)>>,
    input: Option<Box<dyn ResultIterator>>,
    embedding: Arc<[f32]>,
    k: usize,
    _current: Arc<CurrentStorage>,
    vector_property: Option<String>,
}

impl BaselineVectorRerankIterator {
    fn new(
        input: Box<dyn ResultIterator>,
        embedding: Arc<[f32]>,
        k: usize,
        current: Arc<CurrentStorage>,
        property_key: Option<String>,
    ) -> Self {
        let vector_property = property_key.or_else(|| current.get_vector_property_name());
        BaselineVectorRerankIterator {
            sorted: None,
            input: Some(input),
            embedding,
            k,
            _current: current,
            vector_property,
        }
    }

    fn compute_similarity(&self, row: &QueryRow, vector_property: &str) -> Option<f32> {
        let node = row.entity.as_node()?;
        let PropertyValue::Vector(vec) = node.properties.get(vector_property)? else {
            return None;
        };
        let similarity = cosine_similarity(&self.embedding, vec).ok()?;
        if similarity.is_finite() {
            Some(similarity)
        } else {
            None
        }
    }
}

impl ResultIterator for BaselineVectorRerankIterator {
    fn next(&mut self) -> Option<gallifreydb::utils::Result<QueryRow>> {
        if self.sorted.is_none() && self.input.is_some() {
            let vector_property = match &self.vector_property {
                Some(prop) => prop.clone(),
                None => {
                    return Some(Err(gallifreydb::utils::Error::Vector(
                        gallifreydb::utils::error::VectorError::IndexError(
                            "VectorRerank requires a vector index to be enabled.".to_string(),
                        ),
                    )));
                }
            };

            let mut input = self.input.take().unwrap();
            let mut scored: Vec<(QueryRow, f32)> = Vec::new();

            // OLD LOGIC: Collect EVERYTHING into a vector
            while let Some(result) = input.next() {
                match result {
                    Ok(row) => {
                        if let Some(similarity) = self.compute_similarity(&row, &vector_property) {
                            scored.push((row, similarity));
                        }
                    }
                    Err(e) => return Some(Err(e)),
                }
            }

            // OLD LOGIC: Sort the entire vector
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            scored.truncate(self.k);

            self.sorted = Some(scored.into_iter());
        }

        self.sorted.as_mut()?.next().map(|(mut row, score)| {
            row.score = Some(score);
            Ok(row)
        })
    }
}

fn create_test_data(count: usize) -> (Arc<CurrentStorage>, Arc<RwLock<HistoricalStorage>>) {
    let current = Arc::new(CurrentStorage::new());
    let historical = Arc::new(RwLock::new(HistoricalStorage::with_config(
        AnchorConfig::default(),
    )));

    // Enable vector index
    current
        .enable_vector_index("embedding", HnswConfig::new(4, DistanceMetric::Cosine))
        .unwrap();

    // Create nodes with diverse embeddings to ensure sorting isn't trivial
    for i in 0..count {
        // Generate pseudo-random embedding based on i
        let v1 = (i as f32).sin();
        let v2 = (i as f32).cos();
        let v3 = ((i * 2) as f32).sin();
        let v4 = ((i * 2) as f32).cos();
        let embedding = vec![v1, v2, v3, v4];

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

    // Use varying sizes and k values
    for count in [1000, 10_000].iter() {
        for k in [10, 100].iter() {
            let (current, historical) = create_test_data(*count);

            group.throughput(Throughput::Elements(*count as u64));

            // 1. Benchmark Optimized Implementation (using standard execution path)
            group.bench_with_input(
                BenchmarkId::new(format!("optimized_k{}", k), count),
                count,
                |b, &n| {
                    let executor = QueryExecutor::new(current.clone(), historical.clone());
                    b.iter(|| {
                        let plan = PhysicalPlan {
                            root: PhysicalOp::VectorRerank {
                                input: Box::new(PhysicalOp::NodeScan {
                                    label: Some("Person".to_string()),
                                    estimated_rows: n,
                                }),
                                embedding: vec![1.0f32; 4].into(),
                                k: *k,
                                property_key: None,
                            },
                            estimated_cost: Default::default(),
                            temporal_context: None,
                            parallel: false,
                            include_provenance: false,
                        };

                        let results = executor.execute(plan).expect("Execution failed");
                        let _count = results.collect_all().expect("Collection failed").len();
                    })
                },
            );

            // 2. Benchmark Baseline Implementation (using local struct)
            group.bench_with_input(
                BenchmarkId::new(format!("baseline_k{}", k), count),
                count,
                |b, &_n| {
                    // We can't use QueryExecutor for the baseline as it's hardcoded to use the new iterators.
                    // We have to manually construct the iterator chain.
                    b.iter(|| {
                        let input_iter =
                            Box::new(gallifreydb::query::executor::NodeScanIterator::new(
                                Some("Person".to_string()),
                                current.clone(),
                            ));

                        let mut baseline_iter = BaselineVectorRerankIterator::new(
                            input_iter,
                            Arc::from(vec![1.0f32; 4]),
                            *k,
                            current.clone(),
                            None,
                        );

                        // Consume iterator
                        while baseline_iter.next().is_some() {}
                    })
                },
            );
        }
    }

    group.finish();
}

criterion_group!(benches, bench_vector_rerank);
criterion_main!(benches);
