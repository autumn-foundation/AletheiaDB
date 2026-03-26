use aletheiadb::prelude::*;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let db = AletheiaDB::new().unwrap();

    // Basic graph query
    let _results = db.execute_aql(
        "MATCH (n:Person {name: 'Alice'})-[:KNOWS]->(friend:Person) RETURN friend"
    )?;

    // Bi-temporal query (point-in-time)
    let _results = db.execute_aql(
        "AS OF 1705312800000000 MATCH (n:Person {name: 'Alice'}) RETURN n"
    )?;

    Ok(())
}
