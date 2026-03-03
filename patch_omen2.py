import re

with open('src/experimental/omen.rs', 'r') as f:
    content = f.read()

new_content = re.sub(
    r'if vt_start <= time\.wallclock\(\) && vt_start >= best_time \{\s*if let Some\(val\) = v\.properties\.get\(property\)\.and_then\(\|v\| v\.as_vector\(\)\) \{\s*best_vec = Some\(val\.to_vec\(\)\);\s*best_time = vt_start;\s*\}\s*\}',
    r'if vt_start <= time.wallclock() && vt_start >= best_time {\n                #[allow(clippy::collapsible_if)]\n                if let Some(val) = v.properties.get(property).and_then(|v| v.as_vector()) {\n                    best_vec = Some(val.to_vec());\n                    best_time = vt_start;\n                }\n            }',
    content
)

with open('src/experimental/omen.rs', 'w') as f:
    f.write(new_content)
