#[cfg(test)]
mod tests {
    use aletheiadb::core::property::{PropertyMapBuilder, PropertyValue};
    use aletheiadb::storage::current::CurrentStorage;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn fuzz_create_node(label in "\\PC*", key in "\\PC*", val in "\\PC*") {
            let storage = CurrentStorage::new();
            let mut builder = PropertyMapBuilder::new();
            builder = builder.try_insert(&key, PropertyValue::String(val.into())).unwrap();
            let _ = storage.create_node(&label, builder.build());
        }
    }
}
