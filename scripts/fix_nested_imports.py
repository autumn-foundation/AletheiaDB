import os
import re

def fix_nested_error_imports(file_path):
    with open(file_path, 'r', encoding='utf-8') as f:
        content = f.read()

    # Match `use crate::core::error::{... error::VectorError ...}`
    # and replace `error::VectorError` with `VectorError`.

    def replace_nested_error(match):
        block = match.group(1)
        items = [item.strip() for item in block.split(',')]
        new_items = []
        for item in items:
            if item.startswith('error::'):
                new_items.append(item.replace('error::', ''))
            else:
                new_items.append(item)
        return f'use crate::core::error::{{{", ".join(new_items)}}};'

    content = re.sub(r'use crate::core::error::\{([^}]+)\};', replace_nested_error, content)

    with open(file_path, 'w', encoding='utf-8') as f:
        f.write(content)

def scan_and_fix(root_dir):
    for dirpath, _, filenames in os.walk(root_dir):
        for filename in filenames:
            if filename.endswith('.rs'):
                fix_nested_error_imports(os.path.join(dirpath, filename))

if __name__ == "__main__":
    scan_and_fix("src")
