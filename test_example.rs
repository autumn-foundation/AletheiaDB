use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use aletheiadb::core::property::PropertyValue;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = AletheiaDB::new()?;
    let id = db.create_node(
        "User",
        PropertyMapBuilder::new().insert("email", "alice@example.com").build()
    )?;

    let results = db.find_nodes_by_property("User", "email", &PropertyValue::String("alice@example.com".into()));
    assert_eq!(results, vec![id]);
    Ok(())
}
