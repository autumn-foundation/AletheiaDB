use criterion::{Criterion, black_box, criterion_group, criterion_main};
use gallifreydb::core::interning::{InternerConfig, StringInterner};

fn bench_intern_cached(c: &mut Criterion) {
    let interner = StringInterner::new();
    interner.intern("hot_key").unwrap();

    c.bench_function("intern_cached", |b| {
        b.iter(|| interner.intern(black_box("hot_key")))
    });
}

fn bench_intern_new(c: &mut Criterion) {
    let interner = StringInterner::new();
    let mut counter = 0;

    c.bench_function("intern_new", |b| {
        b.iter(|| {
            counter += 1;
            interner.intern(format!("key_{}", counter))
        })
    });
}

fn bench_intern_evicted(c: &mut Criterion) {
    let config = InternerConfig {
        max_cache_size: 1000,
        ..Default::default()
    };
    let interner = StringInterner::with_config(config);

    // Pre-populate and cause eviction
    for i in 0..10_000 {
        interner.intern(format!("key_{}", i)).unwrap();
    }

    c.bench_function("intern_evicted", |b| {
        b.iter(|| interner.intern(black_box("key_0")))
    });
}

criterion_group!(
    benches,
    bench_intern_cached,
    bench_intern_new,
    bench_intern_evicted
);
criterion_main!(benches);
