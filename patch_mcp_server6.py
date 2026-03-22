import re

with open("src/mcp/server.rs", "r") as f:
    content = f.read()

# Add parse_node_id helper
helper = """    fn parse_args<T: serde::de::DeserializeOwned>(&self, args: &serde_json::Value) -> Result<T, CallToolResult> {
        serde_json::from_value(args.clone()).map_err(|e| self.error_json(&format!("Invalid arguments: {e}")))
    }

    fn parse_node_id(&self, id_str: &str) -> Result<NodeId, CallToolResult> {
        NodeId::new(id_str.to_string()).map_err(|e| self.error_json(&e.to_string()))
    }

    fn parse_edge_id(&self, id_str: &str) -> Result<EdgeId, CallToolResult> {
        EdgeId::new(id_str.to_string()).map_err(|e| self.error_json(&e.to_string()))
    }"""
content = content.replace("""    fn parse_args<T: serde::de::DeserializeOwned>(&self, args: &serde_json::Value) -> Result<T, CallToolResult> {
        serde_json::from_value(args.clone()).map_err(|e| self.error_json(&format!("Invalid arguments: {e}")))
    }""", helper)


pattern_node = re.compile(r"""        let (node_id|start_id) = match NodeId::new\((req\.node_id|req\.start_node_id)\) \{\s*Ok\(id\) => id,\s*Err\(e\) => return self\.error_json\(&e\.to_string\(\)\),\s*\};""")

def replace_match_node(match):
    var_name = match.group(1)
    req_field = match.group(2)
    return f"""        let {var_name} = match self.parse_node_id(&{req_field}) {{
            Ok(id) => id,
            Err(err) => return err,
        }};"""

content = pattern_node.sub(replace_match_node, content)

pattern_edge = re.compile(r"""        let edge_id = match EdgeId::new\(req\.edge_id\) \{\s*Ok\(id\) => id,\s*Err\(e\) => return self\.error_json\(&e\.to_string\(\)\),\s*\};""")

def replace_match_edge(match):
    return f"""        let edge_id = match self.parse_edge_id(&req.edge_id) {{
            Ok(id) => id,
            Err(err) => return err,
        }};"""

content = pattern_edge.sub(replace_match_edge, content)

with open("src/mcp/server.rs", "w") as f:
    f.write(content)

print(f"Replaced NodeId/EdgeId matches.")
