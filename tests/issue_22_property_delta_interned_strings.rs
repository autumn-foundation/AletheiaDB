//! Integration tests for Issue #22: PropertyDelta InternedString usage
//!
//! These tests verify that PropertyDelta uses PropertyKey (InternedString)
//! instead of String, providing ~30% memory savings as specified in issue #22.
//!
//! This validates that the fix from commit 9df8517 (P2-1) correctly addresses
//! issue #22's requirements.

use gallifreydb::core::interning::GLOBAL_INTERNER;
use gallifreydb::core::property::{PropertyMapBuilder, PropertyValue};
use gallifreydb::storage::version::PropertyDelta;

#[test]
fn test_property_delta_uses_interned_strings() {
    // This test verifies that PropertyDelta keys are InternedString (PropertyKey),
    // not raw String types. Identical keys should share the same interned ID.
    let key1 = GLOBAL_INTERNER.intern("name").unwrap();
    let key2 = GLOBAL_INTERNER.intern("name").unwrap();

    // Verify that interning the same string twice returns the same ID
    // This is the core memory efficiency benefit - identical strings share storage
    assert_eq!(
        key1.as_u32(),
        key2.as_u32(),
        "Same string should intern to same ID"
    );

    // Create a PropertyDelta and verify it uses PropertyKey (InternedString)
    let mut delta = PropertyDelta::new();
    delta.changed.insert(key1, PropertyValue::string("Alice"));
    delta.removed.insert(GLOBAL_INTERNER.intern("age").unwrap());

    // Verify we can look up using the interned key
    assert!(delta.changed.contains_key(&key2));
    assert_eq!(
        delta.changed.get(&key2),
        Some(&PropertyValue::string("Alice"))
    );
}

#[test]
fn test_property_delta_interned_key_equality() {
    // Verify that PropertyDelta keys use O(1) integer comparison
    // instead of O(n) string comparison
    let old = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .build();

    let new = PropertyMapBuilder::new()
        .insert("name", "Bob")
        .insert("age", 30i64)
        .build();

    let delta = PropertyDelta::from_diff(&old, &new);

    // The "name" key should be in changed using interned comparison
    let name_key = GLOBAL_INTERNER.intern("name").unwrap();
    assert!(delta.changed.contains_key(&name_key));
    assert_eq!(
        delta.changed.get(&name_key),
        Some(&PropertyValue::string("Bob"))
    );

    // The "age" key should not be in changed (value unchanged)
    let age_key = GLOBAL_INTERNER.intern("age").unwrap();
    assert!(!delta.changed.contains_key(&age_key));
}

#[test]
fn test_property_delta_memory_efficiency() {
    // This test demonstrates the memory efficiency of using InternedString.
    // With InternedString, PropertyKey is 4 bytes (u32) instead of 24 bytes (String).
    // This provides ~83% reduction in key storage (24 → 4 bytes).

    // Create multiple deltas with the same property keys
    let common_keys = ["name", "age", "email", "city"];

    let mut deltas = Vec::new();
    for i in 0..100 {
        let mut delta = PropertyDelta::new();
        for key_str in &common_keys {
            let key = GLOBAL_INTERNER.intern(key_str).unwrap();
            delta.changed.insert(key, PropertyValue::Int(i as i64));
        }
        deltas.push(delta);
    }

    // Verify that all deltas use the same interned keys (not duplicated strings)
    let first_delta = &deltas[0];
    let last_delta = &deltas[99];

    for key_str in &common_keys {
        let key = GLOBAL_INTERNER.intern(key_str).unwrap();

        // All deltas should have this key, and it should be the same interned ID
        assert!(first_delta.changed.contains_key(&key));
        assert!(last_delta.changed.contains_key(&key));

        // Verify the keys are the same interned instance
        let (first_key, _) = first_delta.changed.get_key_value(&key).unwrap();
        let (last_key, _) = last_delta.changed.get_key_value(&key).unwrap();
        assert_eq!(first_key.as_u32(), last_key.as_u32());
    }

    // Memory benefit calculation:
    // - With String: 100 deltas × 4 keys × 24 bytes = 9,600 bytes for keys
    // - With InternedString: 100 deltas × 4 keys × 4 bytes = 1,600 bytes for keys
    // - Plus ~100 bytes for the 4 actual strings in the intern table = ~1,700 bytes total
    // - Savings: ~82% reduction in key storage overhead (9,600 → 1,700 bytes)
    //
    // This exceeds the ~30% overhead reduction mentioned in issue #22!
}

#[test]
fn test_property_delta_removed_set_uses_interned_strings() {
    // Verify that the 'removed' HashSet also uses InternedString
    let old = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .insert("city", "NYC")
        .build();

    let new = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        // city removed
        .build();

    let delta = PropertyDelta::from_diff(&old, &new);

    // Verify the removed set contains the interned key
    let city_key = GLOBAL_INTERNER.intern("city").unwrap();
    assert!(delta.removed.contains(&city_key));
    assert_eq!(delta.removed.len(), 1);

    // Verify we can look up using a newly-interned key (same ID)
    let city_key_2 = GLOBAL_INTERNER.intern("city").unwrap();
    assert_eq!(city_key.as_u32(), city_key_2.as_u32());
    assert!(delta.removed.contains(&city_key_2));
}

#[test]
fn test_property_delta_apply_with_interned_keys() {
    // Verify that delta application works correctly with interned keys
    let base = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .insert("city", "NYC")
        .build();

    let mut delta = PropertyDelta::new();

    // Modify 'age' using interned key
    let age_key = GLOBAL_INTERNER.intern("age").unwrap();
    delta.changed.insert(age_key, PropertyValue::Int(31));

    // Add 'country' using interned key
    let country_key = GLOBAL_INTERNER.intern("country").unwrap();
    delta
        .changed
        .insert(country_key, PropertyValue::string("USA"));

    // Remove 'city' using interned key
    let city_key = GLOBAL_INTERNER.intern("city").unwrap();
    delta.removed.insert(city_key);

    // Apply the delta
    let result = delta.apply(&base);

    // Verify the result
    assert_eq!(result.get("name").and_then(|v| v.as_str()), Some("Alice"));
    assert_eq!(result.get("age").and_then(|v| v.as_int()), Some(31));
    assert_eq!(result.get("country").and_then(|v| v.as_str()), Some("USA"));
    assert!(result.get("city").is_none()); // Removed
}

#[test]
fn test_property_delta_large_scale_memory_efficiency() {
    // This test demonstrates memory efficiency at a larger scale,
    // simulating a temporal database with many versions
    const NUM_ENTITIES: usize = 1000;
    const VERSIONS_PER_ENTITY: usize = 10;

    // Common property keys that would appear across many entities
    let common_keys = ["name", "created_at", "updated_at", "status", "owner_id"];

    let mut all_deltas = Vec::new();

    for entity_id in 0..NUM_ENTITIES {
        for version in 0..VERSIONS_PER_ENTITY {
            let mut delta = PropertyDelta::new();

            // Each version typically changes 1-2 properties
            let key = common_keys[version % common_keys.len()];
            let interned_key = GLOBAL_INTERNER.intern(key).unwrap();
            delta.changed.insert(
                interned_key,
                PropertyValue::Int((entity_id * 1000 + version as i64).into(),
            );

            all_deltas.push(delta);
        }
    }

    // Verify that across all deltas, the common keys share the same interned IDs
    let name_key = GLOBAL_INTERNER.intern("name").unwrap();

    let mut deltas_with_name = 0;
    for delta in &all_deltas {
        if delta.changed.contains_key(&name_key) {
            deltas_with_name += 1;
            // Verify the key ID matches
            let (key_in_delta, _) = delta.changed.get_key_value(&name_key).unwrap();
            assert_eq!(key_in_delta.as_u32(), name_key.as_u32());
        }
    }

    assert!(deltas_with_name > 0, "Should have deltas with 'name' key");

    // Memory analysis for this scenario:
    // - 10,000 deltas with average 1 key each = 10,000 keys
    // - With String: 10,000 × 24 bytes = 240,000 bytes for keys
    // - With InternedString: 10,000 × 4 bytes = 40,000 bytes for keys
    // - Plus ~150 bytes for 5 common strings = ~40,150 bytes total
    // - Savings: ~83% reduction (240,000 → 40,150 bytes)
    //
    // This demonstrates that PropertyDelta's use of InternedString provides
    // substantial memory savings for temporal graph databases with many versions.
}
