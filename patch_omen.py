import re

with open('src/experimental/omen.rs', 'r') as f:
    content = f.read()

# Replace the nested if statements with a let chain compatible one or single check
new_content = re.sub(
    r'if vt_start <= time\.wallclock\(\) \{\s*if vt_start >= best_time \{\s*if let Some\(val\) = v\.properties\.get\(property\) \{\s*if let Some\(vec\) = val\.as_vector\(\) \{\s*best_vec = Some\(vec\.to_vec\(\)\);\s*best_time = vt_start;\s*\}\s*\}\s*\}\s*\}',
    r'if vt_start <= time.wallclock() && vt_start >= best_time {\n                if let Some(val) = v.properties.get(property) {\n                    if let Some(vec) = val.as_vector() {\n                        best_vec = Some(vec.to_vec());\n                        best_time = vt_start;\n                    }\n                }\n            }',
    content
)

# Fix further
new_content = re.sub(
    r'if let Some\(val\) = v\.properties\.get\(property\) \{\s*if let Some\(vec\) = val\.as_vector\(\) \{\s*best_vec = Some\(vec\.to_vec\(\)\);\s*best_time = vt_start;\s*\}\s*\}',
    r'if let Some(val) = v.properties.get(property).and_then(|v| v.as_vector()) {\n                    best_vec = Some(val.to_vec());\n                    best_time = vt_start;\n                }',
    new_content
)

with open('src/experimental/omen.rs', 'w') as f:
    f.write(new_content)
