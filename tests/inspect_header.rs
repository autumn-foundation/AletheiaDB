use aletheiadb::index::vector::{HnswIndexBuilder, DistanceMetric};
use aletheiadb::core::id::NodeId;
use std::fs::File;
use std::io::Read;

fn main() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("valid.index");

    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build().unwrap();
    index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0]).unwrap();
    index.save(&path).unwrap();

    let mut file = File::open(&path).unwrap();
    let mut buffer = [0u8; 16];
    file.read_exact(&mut buffer).unwrap();

    println!("Header: {:?}", buffer);
}
