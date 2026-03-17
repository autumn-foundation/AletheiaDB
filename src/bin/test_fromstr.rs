use std::str::FromStr;
use aletheiadb::prelude::*;

fn main() {
    let id = NodeId::from_str("42");
    println!("{:?}", id);
}
