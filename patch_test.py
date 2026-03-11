import os
filepath = 'src/sql/temporal_parser.rs'
with open(filepath, 'r') as f:
    content = f.read()

old_str = '    cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");'
new_str = '''    // ⚡ Bolt: Avoid allocating an intermediate Vec just to join string slices.
    let mut new_cleaned = String::with_capacity(cleaned.len());
    let mut first = true;
    for word in cleaned.split_whitespace() {
        if !first {
            new_cleaned.push(' ');
        }
        new_cleaned.push_str(word);
        first = false;
    }
    cleaned = new_cleaned;'''

if old_str in content:
    with open(filepath, 'w') as f:
        f.write(content.replace(old_str, new_str))
    print("Patched successfully")
else:
    print("Could not find string to replace")
