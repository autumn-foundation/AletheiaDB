use aletheiadb::query::parser::Parser;
use aletheiadb::query::lexer::Token;
fn main() {
    let result = Parser::parse("MATCH (a) (b) RETURN a");
    println!("{:?}", result);
}
