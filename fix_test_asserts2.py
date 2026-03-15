import re

with open('src/mcp/server.rs', 'r') as f:
    text = f.read()

pattern = r'assert!\(\n\s*result\.is_error,\n\s*"handle_([a-zA-Z0-9_]+) failed.*?"\n\s*\);'
replacement = r'''assert!(
            result.is_error.unwrap_or(false),
            "handle_\1 failed to return an error for invalid args"
        );'''

new_text = re.sub(pattern, replacement, text)

with open('src/mcp/server.rs', 'w') as f:
    f.write(new_text)
