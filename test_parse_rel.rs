use aletheiadb::query::parser::Parser;
fn main() {
    let result = Parser::parse("MATCH (a)-[:REL*0..2]->(b) RETURN a");
    println!("{:?}", result);
}
