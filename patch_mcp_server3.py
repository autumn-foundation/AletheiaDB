import re

with open("src/mcp/server.rs", "r") as f:
    content = f.read()

# Add parse_args helper
helper = """    fn error_json(&self, msg: &str) -> CallToolResult {
        CallToolResult::error(vec![Content::text(json!({"error": msg}).to_string())])
    }

    fn parse_args<T: serde::de::DeserializeOwned>(&self, args: &serde_json::Value) -> Result<T, CallToolResult> {
        serde_json::from_value(args.clone()).map_err(|e| self.error_json(&format!("Invalid arguments: {e}")))
    }"""
content = content.replace("""    fn error_json(&self, msg: &str) -> CallToolResult {
        CallToolResult::error(vec![Content::text(json!({"error": msg}).to_string())])
    }""", helper)

# Refactor the match self.parse_args block using guard clauses
pattern = re.compile(r"""        let req: ([a-zA-Z0-9_<>]+) = match serde_json::from_value\(args\) \{\s*Ok\(r\) => r,\s*Err\(e\) => return self\.error_json\(&format!\("Invalid arguments: \{\}", e\)\),\s*\};""")

def replace_match_guard(match):
    type_name = match.group(1)
    return f"""        let req = match self.parse_args::<{type_name}>(&args) {{
            Ok(req) => req,
            Err(err) => return err,
        }};"""

new_content = pattern.sub(replace_match_guard, content)

with open("src/mcp/server.rs", "w") as f:
    f.write(new_content)

print(f"Replaced {len(pattern.findall(content))} matches using parse_args helper.")
