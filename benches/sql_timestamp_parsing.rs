//! Benchmarks for SQL timestamp parsing
//!
//! Measures performance of various timestamp formats supported by
//! TemporalClause::parse_timestamp() to ensure:
//! - Unix microseconds remain the fast path
//! - ISO 8601 parsing performance is acceptable
//! - No regressions in parsing logic

#[cfg(feature = "sql")]
use criterion::{black_box, criterion_group, criterion_main, Criterion};
#[cfg(feature = "sql")]
use gallifreydb::sql::TemporalClause;

#[cfg(feature = "sql")]
fn bench_timestamp_parsing(c: &mut Criterion) {
    let mut group = c.benchmark_group("timestamp_parsing");

    // Fast path: Unix microseconds (should be fastest)
    group.bench_function("unix_microseconds", |b| {
        b.iter(|| {
            black_box(TemporalClause::parse_timestamp(black_box(
                "1705312800000000",
            )))
        })
    });

    group.bench_function("unix_microseconds_quoted", |b| {
        b.iter(|| {
            black_box(TemporalClause::parse_timestamp(black_box(
                "'1705312800000000'",
            )))
        })
    });

    // ISO 8601 UTC format
    group.bench_function("iso8601_utc", |b| {
        b.iter(|| {
            black_box(TemporalClause::parse_timestamp(black_box(
                "2024-01-15T10:00:00Z",
            )))
        })
    });

    group.bench_function("iso8601_utc_fractional", |b| {
        b.iter(|| {
            black_box(TemporalClause::parse_timestamp(black_box(
                "2024-01-15T10:00:00.123456Z",
            )))
        })
    });

    // ISO 8601 with timezone offset
    group.bench_function("iso8601_offset", |b| {
        b.iter(|| {
            black_box(TemporalClause::parse_timestamp(black_box(
                "2024-01-15T15:30:00+05:30",
            )))
        })
    });

    // SQL timestamp format
    group.bench_function("sql_format", |b| {
        b.iter(|| {
            black_box(TemporalClause::parse_timestamp(black_box(
                "2024-01-15 10:00:00",
            )))
        })
    });

    group.bench_function("sql_format_fractional", |b| {
        b.iter(|| {
            black_box(TemporalClause::parse_timestamp(black_box(
                "2024-01-15 10:00:00.123456",
            )))
        })
    });

    // Naive datetime with T separator
    group.bench_function("naive_datetime_t_separator", |b| {
        b.iter(|| {
            black_box(TemporalClause::parse_timestamp(black_box(
                "2024-01-15T10:00:00",
            )))
        })
    });

    // Date-only format
    group.bench_function("date_only", |b| {
        b.iter(|| {
            black_box(TemporalClause::parse_timestamp(black_box(
                "2024-01-15",
            )))
        })
    });

    // Error path: invalid format (important to measure error handling overhead)
    group.bench_function("invalid_format", |b| {
        b.iter(|| {
            black_box(TemporalClause::parse_timestamp(black_box(
                "not-a-timestamp",
            )))
        })
    });

    group.finish();
}

#[cfg(feature = "sql")]
criterion_group!(benches, bench_timestamp_parsing);

#[cfg(feature = "sql")]
criterion_main!(benches);

#[cfg(not(feature = "sql"))]
fn main() {
    println!("Benchmark requires 'sql' feature flag");
}
