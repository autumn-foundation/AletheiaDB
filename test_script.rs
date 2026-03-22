use std::fs;

fn main() {
    let content = fs::read_to_string("src/query/planner/rules/operation_reordering.rs").unwrap();
    let new_content = content.replace(
        "(Predicate::Ne { key: k1, value: v1 }, Predicate::Ne { key: k2, value: v2 }) => {\n                k1 == k2 && v1 == v2\n            }",
        "(Predicate::Ne { key: k1, value: v1 }, Predicate::Ne { key: k2, value: v2 }) => { false }"
    );
    fs::write("src/query/planner/rules/operation_reordering.rs", new_content).unwrap();
}
