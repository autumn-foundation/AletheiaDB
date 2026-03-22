import re

with open("src/mcp/server.rs", "r") as f:
    content = f.read()

# Replace match with guard clauses avoiding unwrap_err running parsing twice
pattern = re.compile(r"""        let Ok\(req\) = self\.parse_args::<([a-zA-Z0-9_<>]+)>\(&args\) else \{\s*return self\.parse_args::<[a-zA-Z0-9_<>]+>\(&args\)\.unwrap_err\(\);\s*\};""")

def replace_match_guard(match):
    type_name = match.group(1)
    return f"""        let req = match self.parse_args::<{type_name}>(&args) {{
            Ok(req) => req,
            Err(err) => return err,
        }};"""

new_content = pattern.sub(replace_match_guard, content)

with open("src/mcp/server.rs", "w") as f:
    f.write(new_content)

print(f"Reverted {len(pattern.findall(content))} matches back to match block to avoid double parsing.")
