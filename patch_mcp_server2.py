import re

with open("src/mcp/server.rs", "r") as f:
    content = f.read()

pattern = re.compile(r"""        let req: ([a-zA-Z0-9_<>]+) = match self\.parse_args\(args\) \{\s*Ok\(req\) => req,\s*Err\(err_res\) => return err_res,\s*\};""")

def replace_match(match):
    type_name = match.group(1)
    return f"""        let req: {type_name} = match self.parse_args(args) {{
            Ok(req) => req,
            Err(err_res) => return err_res,
        }};"""

def replace_match_guard(match):
    type_name = match.group(1)
    return f"""        let Ok(req) = self.parse_args::<{type_name}>(args) else {{
            return self.parse_args::<{type_name}>(args).unwrap_err();
        }};"""

new_content = pattern.sub(replace_match_guard, content)

with open("src/mcp/server.rs", "w") as f:
    f.write(new_content)

print(f"Replaced {len(pattern.findall(content))} matches using guard clauses.")
