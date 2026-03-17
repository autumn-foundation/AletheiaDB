use aletheiadb::prelude::*;

fn main() {
    let _ = std::fs::remove_dir_all("aletheiadb");
    let db = AletheiaDB::new().unwrap();
    let res = db.create_edge(NodeId::new(1).unwrap(), NodeId::new(2).unwrap(), "KNOWS", properties!{});
    println!("Error: {:?}", res);
}
