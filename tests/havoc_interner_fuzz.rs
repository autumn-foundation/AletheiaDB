use aletheiadb::AletheiaDB;
use aletheiadb::core::property::PropertyMapBuilder;
use proptest::prelude::*;
use std::sync::Arc;

lazy_static::lazy_static! {
    static ref DB: Arc<AletheiaDB> = Arc::new(AletheiaDB::new().unwrap());
}

proptest! {
    #[test]
    fn fuzz_create_node_label(label in "\\PC*", key in "\\PC*", value in "\\PC*") {
        // The previous test instantiated a DB per iteration, which is too slow.
        // We use a shared static DB now.
        let db = &*DB;

        // We use insert() (which internally used to panic on interner exhaustion)
        // to verify that the application code safely swallows the error rather than crashing.
        let props = PropertyMapBuilder::new().insert(&key, value).build();
        let _ = db.create_node(&label, props);
    }
}
