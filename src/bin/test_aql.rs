use aletheiadb::prelude::*;
use aletheiadb::query::parse_query;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let _ = std::fs::remove_dir_all("aletheiadb");
    let db = AletheiaDB::new().unwrap();
    let n1 = db.create_node("Person", properties!{"name" => "Alice"})?;
    let n2 = db.create_node("Person", properties!{"name" => "Bob"})?;
    db.create_edge(n1, n2, "KNOWS", properties!{})?;

    // currently we do:
    let q = parse_query("MATCH (n:Person)-[:KNOWS]->(friend) RETURN friend LIMIT 10")?;
    let results = db.execute_query(q)?;

    for row in results {
        println!("{:?}", row?.entity.id());
    }

    Ok(())
}
