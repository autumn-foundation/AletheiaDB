use aletheiadb::db::AletheiaDB;
use proptest::prelude::*;

proptest! {
    #[test]
    fn test_db_create_node_proptest(label in "\\PC*") {
        let db = AletheiaDB::new().unwrap();
        let prop_map = aletheiadb::core::property::PropertyMap::new();
        // Just checking that random labels don't panic
        let _ = db.create_node(&label, prop_map);
    }
}
