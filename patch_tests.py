import re

with open("src/mcp/tests.rs", "r") as f:
    content = f.read()

content = content.replace("fn test_get_node() {", "#[test]\n    fn test_get_node() {")

with open("src/mcp/tests.rs", "w") as f:
    f.write(content)

print(f"Fixed missing test macro.")
