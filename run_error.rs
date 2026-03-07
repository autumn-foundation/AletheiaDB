use aletheiadb::prelude::*;

fn main() {
    let db = AletheiaDB::new().unwrap();
    // Try to get a node that doesn't exist
    let result = db.get_node(NodeId(999999));
    println!("{:?}", result);
}
