#[cfg(test)]
mod tests {
    use gallifreydb::core::interning::GLOBAL_INTERNER;
    use gallifreydb::core::property::PropertyMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_key() -> String {
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("unique_leak_test_key_{}", count)
    }

    #[test]
    fn test_property_map_get_does_not_leak_interned_strings() {
        let map = PropertyMap::new();

        // Calling get with a non-existent, non-interned string
        let random_key = unique_key();

        // Ensure it's not already there
        if GLOBAL_INTERNER.contains(&random_key) {
            panic!("Unique key collision in test setup");
        }

        let _ = map.get(&random_key);

        // Should not have interned the string
        assert!(
            !GLOBAL_INTERNER.contains(&random_key),
            "PropertyMap::get leaked a string into the interner!"
        );
    }

    #[test]
    fn test_property_map_contains_key_does_not_leak_interned_strings() {
        let map = PropertyMap::new();

        let random_key = unique_key();

        if GLOBAL_INTERNER.contains(&random_key) {
            panic!("Unique key collision in test setup");
        }

        let _ = map.contains_key(&random_key);

        // Should not have interned the string
        assert!(
            !GLOBAL_INTERNER.contains(&random_key),
            "PropertyMap::contains_key leaked a string into the interner!"
        );
    }
}
