import re

with open("src/mcp/server.rs", "r") as f:
    content = f.read()

# Update parse_node_id / parse_edge_id to take u64 instead of &str
helper = """    fn parse_node_id(&self, id: u64) -> Result<NodeId, CallToolResult> {
        NodeId::new(id).map_err(|e| self.error_json(&e.to_string()))
    }

    fn parse_edge_id(&self, id: u64) -> Result<EdgeId, CallToolResult> {
        EdgeId::new(id).map_err(|e| self.error_json(&e.to_string()))
    }"""
content = content.replace("""    fn parse_node_id(&self, id_str: &str) -> Result<NodeId, CallToolResult> {
        NodeId::new(id_str.to_string()).map_err(|e| self.error_json(&e.to_string()))
    }

    fn parse_edge_id(&self, id_str: &str) -> Result<EdgeId, CallToolResult> {
        EdgeId::new(id_str.to_string()).map_err(|e| self.error_json(&e.to_string()))
    }""", helper)

# Remove the & from &req.node_id and &req.edge_id in calls
content = re.sub(r'self\.parse_node_id\(&([^)]+)\)', r'self.parse_node_id(\1)', content)
content = re.sub(r'self\.parse_edge_id\(&([^)]+)\)', r'self.parse_edge_id(\1)', content)

with open("src/mcp/server.rs", "w") as f:
    f.write(content)

print(f"Fixed NodeId/EdgeId expected type.")
