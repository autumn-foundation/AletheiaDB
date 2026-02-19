
import os
import re

def find_missing_docs(filepath):
    with open(filepath, 'r') as f:
        lines = f.readlines()

    missing_docs = []

    # Regex for public items
    # Captures: pub struct, pub enum, pub fn, pub trait, pub mod, pub type, pub const, pub static
    # It handles attributes like #[...] before the item
    pub_item_pattern = re.compile(r'^\s*(pub(?:\([^\)]+\))?)\s+(struct|enum|fn|trait|mod|type|const|static)\s+(\w+)')

    # Regex for doc comments
    doc_comment_pattern = re.compile(r'^\s*///')

    # Regex for attributes
    attribute_pattern = re.compile(r'^\s*#\[')

    for i, line in enumerate(lines):
        match = pub_item_pattern.match(line)
        if match:
            item_type = match.group(2)
            item_name = match.group(3)

            # Check previous lines for doc comments
            has_doc = False
            j = i - 1
            while j >= 0:
                prev_line = lines[j]
                if doc_comment_pattern.match(prev_line):
                    has_doc = True
                    break
                elif attribute_pattern.match(prev_line):
                    j -= 1
                    continue
                elif not prev_line.strip(): # Empty line
                    j -= 1
                    continue
                else:
                    break # Found something else (another code line, comment, etc)

            if not has_doc:
                missing_docs.append((i + 1, f"{item_type} {item_name}"))

    return missing_docs

def main():
    print(f"{'File':<60} {'Line':<10} {'Item'}")
    print("-" * 90)

    count = 0
    for root, dirs, files in os.walk('src'):
        for file in files:
            if file.endswith('.rs'):
                filepath = os.path.join(root, file)
                missing = find_missing_docs(filepath)
                for line_num, item in missing:
                    print(f"{filepath:<60} {line_num:<10} {item}")
                    count += 1
                    if count >= 50: # Limit output
                        print("...")
                        return

if __name__ == "__main__":
    main()
