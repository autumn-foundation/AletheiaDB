import re

with open("src/core/id.rs", "r") as f:
    content = f.read()

# Find the test at the end of the file
test_pattern = r"\n    #\[test\]\n    fn test_tx_id_generator_next_mutant_kill\(\) \{.*?\n    \}\n"
match = re.search(test_pattern, content, re.DOTALL)

if match:
    test_str = match.group(0)
    # Remove it from the end
    content = content.replace(test_str, "")

    # Insert it inside the sentinel_id_generator_tests module
    module_pattern = r"(mod sentinel_id_generator_tests \{\n    use super::\*;\n)"
    content = re.sub(module_pattern, r"\1" + test_str, content)

    with open("src/core/id.rs", "w") as f:
        f.write(content)
else:
    print("Test not found!")
