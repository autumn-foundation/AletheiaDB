use aletheiadb::prelude::*;
fn main() {
    let db = AletheiaDB::new().unwrap();
    let err = db.get_node(NodeId::new_unchecked(999)).unwrap_err();
    println!("Error message: {}", err);
}
