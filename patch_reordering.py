import re
with open("src/query/planner/rules/operation_reordering.rs", "r") as f:
    content = f.read()

new_pred_equal = """    fn predicates_equal(&self, a: &Predicate, b: &Predicate) -> bool {
        a == b
    }"""
content = re.sub(r'    fn predicates_equal\(&self, a: &Predicate, b: &Predicate\) -> bool \{.*?\n    \}', new_pred_equal, content, flags=re.DOTALL)

with open("src/query/planner/rules/operation_reordering.rs", "w") as f:
    f.write(content)
