use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use gallifreydb::core::id::{NodeId, VersionId};
use gallifreydb::core::temporal::{BiTemporalInterval, TimeRange, Timestamp};
use gallifreydb::index::temporal::TemporalIndexes;

fn bench_valid_at_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("valid_at_query");

    // Test with 100, 1K, 10K versions per entity
    for version_count in [100, 1000, 10000] {
        let indexes = TemporalIndexes::new();
        let node_id = NodeId::new(1).unwrap();

        // Insert sequential versions
        // Each version is valid for 1000 ticks
        for i in 0..version_count {
            let start = i * 1000;
            let end = (i + 1) * 1000;
            let v_id = VersionId::new(i as u64).unwrap();

            indexes.insert_node_version(
                node_id,
                v_id,
                BiTemporalInterval::new(
                    TimeRange::new(start, end),
                    TimeRange::from(0), // Tx time is irrelevant for this test
                ),
            );
        }

        // Query for a time in the middle of the history
        let query_time = (version_count / 2 * 1000) + 500; // Middle of a version

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}_versions", version_count)),
            &query_time,
            |b, &time| {
                b.iter(|| {
                    // New efficient query: "valid at time T"
                    // Since the index now supports overlaps, we can just query the point interval.
                    let range = TimeRange::new(time, time + 1);
                    let valid = indexes.find_node_versions_in_valid_time_range(node_id, range);
                    black_box(valid)
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_valid_at_query);
criterion_main!(benches);
