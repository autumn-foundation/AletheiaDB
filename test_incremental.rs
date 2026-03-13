use aletheiadb::core::id::{IdGenerator, MAX_VALID_ID};

fn main() {
    let gen = IdGenerator::with_start(MAX_VALID_ID);
    println!("hello");
}
