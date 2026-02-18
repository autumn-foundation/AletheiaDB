import os
import re

def fix_imports(file_path):
    with open(file_path, 'r', encoding='utf-8') as f:
        content = f.read()

    # Replace `use crate::utils::{...}` with `use crate::core::error::{...}`
    # This handles simple cases. Complex multi-line imports might need more care.
    # Regex to capture content inside {}

    # 1. Simple replacements
    content = content.replace('use crate::utils::error::', 'use crate::core::error::')
    content = content.replace('use crate::utils::Error;', 'use crate::core::error::Error;')
    content = content.replace('use crate::utils::Result;', 'use crate::core::error::Result;')

    # 2. Block imports
    # use crate::utils::{Error, Result};
    # We want to change `crate::utils` to `crate::core::error` if the block contains error types.
    # If it contains other things (which utils didn't seem to have), we might be in trouble.
    # But `utils` only had `error`.

    content = re.sub(r'use crate::utils::\{', 'use crate::core::error::{', content)

    # 3. `use crate::utils;` -> `use crate::core::error as utils;` ? No, better to fix usage.
    # But checking previous errors: `use crate::utils::{Error, Result, error::VectorError};`
    # This implies nested `error`.
    # `utils::error::VectorError` -> `core::error::VectorError`.
    # `utils::{..., error::VectorError}` -> `core::error::{..., VectorError}`.

    # Let's fix specific lines found in errors.

    # Fix: `use crate::utils::{Error, Result, error::VectorError};`
    # -> `use crate::core::error::{Error, Result, VectorError};`
    # because VectorError is now directly in core::error (re-exported or defined there).

    # Wait, `src/core/error.rs` defines `VectorError`.
    # So `crate::core::error::VectorError` is correct.
    # `crate::utils::error::VectorError` was correct because `utils` had `pub mod error`.

    # So `error::VectorError` inside `utils::{...}` should become `VectorError` inside `core::error::{...}`.

    def replace_utils_block(match):
        block = match.group(1)
        # Remove `error::` prefix from items in the block
        items = [item.strip().replace('error::', '') for item in block.split(',')]
        return f'use crate::core::error::{{{", ".join(items)}}};'

    content = re.sub(r'use crate::utils::\{([^}]+)\};', replace_utils_block, content)

    with open(file_path, 'w', encoding='utf-8') as f:
        f.write(content)

def scan_and_fix(root_dir):
    for dirpath, _, filenames in os.walk(root_dir):
        for filename in filenames:
            if filename.endswith('.rs'):
                fix_imports(os.path.join(dirpath, filename))

if __name__ == "__main__":
    scan_and_fix("src")
