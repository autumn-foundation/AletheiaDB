use aletheiadb::prelude::*;
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let db = AletheiaDB::new().unwrap();
    let valid_time = "2024-01-01"; // Wrong type
    let _results = db.query()
        .as_of(valid_time, valid_time)
        .start(NodeId::new(1).unwrap())
        .traverse("KNOWS")
        .execute(&db)?;
    Ok(())
}
