with open('src/mcp/server.rs', 'r') as f:
    text = f.read()

import re

# We need to add assertions to the test we added
pattern = r'let _ = self\.handle_([a-zA-Z0-9_]+)\(invalid_args\.clone\(\)\);'
replacement = r'''let result = self.handle_\1(invalid_args.clone());
        assert!(result.is_error, "handle_\1 failed to return an error for invalid args");'''

new_text = re.sub(pattern, replacement, text)

with open('src/mcp/server.rs', 'w') as f:
    f.write(new_text)
