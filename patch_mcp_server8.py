import re

with open("src/mcp/server.rs", "r") as f:
    content = f.read()

# Replace parse_node_id/parse_edge_id matches with guard clauses
pattern_node = re.compile(r"""        let ([a-zA-Z0-9_]+) = match self\.parse_node_id\((req\.[a-zA-Z0-9_]+)\) \{\s*Ok\(id\) => id,\s*Err\(err\) => return err,\s*\};""")

def replace_match_node(match):
    var_name = match.group(1)
    req_field = match.group(2)
    return f"""        let Ok({var_name}) = self.parse_node_id({req_field}) else {{
            return self.parse_node_id({req_field}).unwrap_err();
        }};"""

content = pattern_node.sub(replace_match_node, content)

pattern_edge = re.compile(r"""        let ([a-zA-Z0-9_]+) = match self\.parse_edge_id\((req\.[a-zA-Z0-9_]+)\) \{\s*Ok\(id\) => id,\s*Err\(err\) => return err,\s*\};""")

def replace_match_edge(match):
    var_name = match.group(1)
    req_field = match.group(2)
    return f"""        let Ok({var_name}) = self.parse_edge_id({req_field}) else {{
            return self.parse_edge_id({req_field}).unwrap_err();
        }};"""

content = pattern_edge.sub(replace_match_edge, content)

with open("src/mcp/server.rs", "w") as f:
    f.write(content)

print(f"Replaced NodeId/EdgeId matches with guard clauses.")
