use criterion::{black_box, criterion_group, criterion_main, Criterion};
use dashmap::DashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::hash::{BuildHasherDefault, Hasher};

// Minimal reproduction of StringInterner logic
#[derive(Default)]
pub struct IdentityHasher(u64);
impl Hasher for IdentityHasher {
    fn write(&mut self, bytes: &[u8]) { self.0 = 0; } // simplified
    fn write_u32(&mut self, i: u32) { self.0 = i as u64; }
    fn finish(&self) -> u64 { self.0 }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct InternedString(u32);
impl InternedString {
    pub const fn as_u32(self) -> u32 { self.0 }
}

pub struct StringInterner {
    id_to_string: DashMap<InternedString, Arc<str>, BuildHasherDefault<IdentityHasher>>,
    next_id: AtomicU32,
}

impl StringInterner {
    pub fn new() -> Self {
        Self {
            id_to_string: DashMap::with_hasher(BuildHasherDefault::default()),
            next_id: AtomicU32::new(0),
        }
    }

    pub fn intern(&self, s: &str) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.id_to_string.insert(InternedString(id), Arc::from(s));
    }

    pub fn len(&self) -> usize {
        self.id_to_string.len()
    }

    // Current implementation
    pub fn get_all_strings_current(&self) -> Vec<String> {
        let count = self.len();
        let mut strings = Vec::with_capacity(count);

        for entry in self.id_to_string.iter() {
            let id = entry.key().as_u32() as usize;
            if id >= strings.len() {
                strings.resize(id + 1, String::new());
            }
            strings[id] = entry.value().to_string();
        }
        strings
    }

    // Proposed optimization
    pub fn get_all_strings_optimized(&self) -> Vec<String> {
        let max_id = self.next_id.load(Ordering::Relaxed) as usize;
        let mut strings = vec![String::new(); max_id];

        for entry in self.id_to_string.iter() {
            let id = entry.key().as_u32() as usize;
            if id < strings.len() {
                strings[id] = entry.value().to_string();
            } else {
                if id >= strings.len() {
                    strings.resize(id + 1, String::new());
                }
                strings[id] = entry.value().to_string();
            }
        }
        strings
    }
}

fn bench_get_all_strings(c: &mut Criterion) {
    let interner = StringInterner::new();
    let n = 100_000;
    for i in 0..n {
        interner.intern(&format!("string_{}", i));
    }

    let mut group = c.benchmark_group("get_all_strings");

    group.bench_function("current", |b| {
        b.iter(|| {
            black_box(interner.get_all_strings_current());
        })
    });

    group.bench_function("optimized", |b| {
        b.iter(|| {
            black_box(interner.get_all_strings_optimized());
        })
    });

    group.finish();
}

criterion_group!(benches, bench_get_all_strings);
criterion_main!(benches);
