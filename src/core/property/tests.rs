
    #[test]
    fn test_serialize_recursion_limit() {
        let mut value = PropertyValue::Null;
        for _ in 0..=MAX_RECURSION_DEPTH {
            value = PropertyValue::Array(Arc::new(vec![value]));
        }

        let result = value.serialize();
        assert!(result.is_err());
        match result {
            Err(crate::utils::error::Error::Storage(StorageError::CorruptedData(msg))) => {
                assert!(msg.contains("recursion depth limit exceeded"));
            }
            _ => panic!("Expected CorruptedData error for recursion limit"),
        }
    }

    #[test]
    fn test_serialized_size_recursion_limit() {
        let mut value = PropertyValue::Null;
        for _ in 0..=MAX_RECURSION_DEPTH {
            value = PropertyValue::Array(Arc::new(vec![value]));
        }

        let result = value.serialized_size();
        assert!(result.is_err());
        match result {
            Err(crate::utils::error::Error::Storage(StorageError::CorruptedData(msg))) => {
                assert!(msg.contains("recursion depth limit exceeded"));
            }
            _ => panic!("Expected CorruptedData error for recursion limit"),
        }
    }

    #[test]
    fn test_estimated_heap_size_recursion_limit() {
        let mut value = PropertyValue::Null;
        for _ in 0..=MAX_RECURSION_DEPTH {
            value = PropertyValue::Array(Arc::new(vec![value]));
        }

        // estimated_heap_size returns a large value on error (no Result)
        let size = value.estimated_heap_size();
        assert!(size >= 10 * 1024 * 1024);
    }

    #[test]
    fn test_map_insert_by_key() {
        let interned_key = GLOBAL_INTERNER.intern("key").unwrap();
        let map = PropertyMapBuilder::new()
            .insert_by_key(interned_key, PropertyValue::Int(42))
            .build();

        assert_eq!(map.get("key").and_then(|v| v.as_int()), Some(42));
    }

    #[test]
    fn test_map_remove_by_key() {
        let interned_key = GLOBAL_INTERNER.intern("key").unwrap();
        let map = PropertyMapBuilder::new()
            .insert("key", 42)
            .remove_by_key(&interned_key)
            .build();

        assert!(map.is_empty());
    }

    #[test]
    fn test_map_try_insert() {
        let result = PropertyMapBuilder::new()
            .try_insert("key", 42);
        assert!(result.is_ok());
    }

    #[test]
    fn test_map_try_remove() {
        let result = PropertyMapBuilder::new()
            .insert("key", 42)
            .try_remove("key");
        assert!(result.is_ok());
    }
}
