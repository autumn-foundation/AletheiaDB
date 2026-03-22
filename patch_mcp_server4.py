import re

with open("src/mcp/server.rs", "r") as f:
    content = f.read()

# Replace match with guard clauses
pattern = re.compile(r"""        let req = match self\.parse_args::<([a-zA-Z0-9_<>]+)>\(&args\) \{\s*Ok\(req\) => req,\s*Err\(err\) => return err,\s*\};""")

def replace_match_guard(match):
    type_name = match.group(1)
    return f"""        let Ok(req) = self.parse_args::<{type_name}>(&args) else {{
            return self.parse_args::<{type_name}>(&args).unwrap_err();
        }};"""

new_content = pattern.sub(replace_match_guard, content)

with open("src/mcp/server.rs", "w") as f:
    f.write(new_content)

print(f"Replaced {len(pattern.findall(content))} matches with guard clauses.")
