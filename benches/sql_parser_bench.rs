#[cfg(feature = "sql")]
use criterion::{Criterion, black_box, criterion_group, criterion_main};
#[cfg(feature = "sql")]
use gallifreydb::sql::extract_temporal_clauses;

#[cfg(feature = "sql")]
fn bench_extract_temporal_clauses(c: &mut Criterion) {
    let mut group = c.benchmark_group("sql_parser");

    let sql = "SELECT * FROM nodes   FOR   SYSTEM_TIME   AS OF   TIMESTAMP  '1000'   WHERE   age   >   21";

    group.bench_function("extract_temporal_clauses_whitespace", |b| {
        b.iter(|| black_box(extract_temporal_clauses(black_box(sql))))
    });

    group.finish();
}

#[cfg(feature = "sql")]
criterion_group!(benches, bench_extract_temporal_clauses);

#[cfg(feature = "sql")]
criterion_main!(benches);

#[cfg(not(feature = "sql"))]
fn main() {
    println!("Benchmark requires 'sql' feature flag");
}
