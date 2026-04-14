use aletheiadb::AletheiaDB;
use proptest::prelude::*;
use std::sync::LazyLock;

static DB: LazyLock<AletheiaDB> = LazyLock::new(|| AletheiaDB::new().unwrap());

proptest! {
    #[test]
    fn test_execute_aql_fuzz_large(q in any::<String>()) {
        // AQL parser might panic! Use global db to avoid re-creation
        let _ = DB.execute_aql(&q);
    }
}
