import re

with open("src/mcp/server.rs", "r") as f:
    content = f.read()

# Add parse_args helper
helper = """    fn error_json(&self, msg: &str) -> CallToolResult {
        CallToolResult::error(vec![Content::text(json!({"error": msg}).to_string())])
    }

    fn parse_args<T: serde::de::DeserializeOwned>(&self, args: serde_json::Value) -> Result<T, CallToolResult> {
        serde_json::from_value(args).map_err(|e| self.error_json(&format!("Invalid arguments: {e}")))
    }"""
content = content.replace("""    fn error_json(&self, msg: &str) -> CallToolResult {
        CallToolResult::error(vec![Content::text(json!({"error": msg}).to_string())])
    }""", helper)

# Now find and replace all match serde_json::from_value
pattern = re.compile(r"""        let req:\s*([a-zA-Z0-9_<>]+)\s*=\s*match\s*serde_json::from_value\(args\)\s*\{\s*Ok\(r\)\s*=>\s*r,\s*Err\(e\)\s*=>\s*return\s*self\.error_json\(&format!\("Invalid arguments: \{\}",\s*e\)\),\s*\};""")

def replace_match(match):
    type_name = match.group(1)
    return f"""        let req: {type_name} = match self.parse_args(args) {{
            Ok(req) => req,
            Err(err_res) => return err_res,
        }};"""

new_content = pattern.sub(replace_match, content)

with open("src/mcp/server.rs", "w") as f:
    f.write(new_content)

print(f"Replaced {len(pattern.findall(content))} matches.")
